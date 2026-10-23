//! Read-only LevelDB reader (write-ahead logs + sorted tables + Snappy), just enough to
//! read other applications' databases such as Hydra Launcher's library. Nothing here ever
//! writes, and a damaged or half-written file only costs the entries it held.
//!
//! The newest entry for each key wins (sequence numbers are global across files) and
//! deletion markers remove keys, exactly as the LevelDB engine itself would resolve them.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

const TABLE_MAGIC: u64 = 0xdb47_7524_8b80_fb57;
const LOG_BLOCK: usize = 32768;

/// Raw key/value pairs.
pub type Pairs = Vec<(Vec<u8>, Vec<u8>)>;
/// key → (sequence, value or None when deleted)
type Latest = HashMap<Vec<u8>, (u64, Option<Vec<u8>>)>;

fn varint(buf: &[u8], pos: &mut usize) -> Result<u64, String> {
    let (mut v, mut shift) = (0u64, 0u32);
    loop {
        let b = *buf.get(*pos).ok_or("truncated varint")?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err("varint too long".into());
        }
    }
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], String> {
    let end = pos.checked_add(n).filter(|e| *e <= buf.len()).ok_or("truncated data")?;
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

/// Raw Snappy block decompression.
pub fn snappy_decompress(src: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 0;
    let want = varint(src, &mut pos)? as usize;
    if want > 64 * 1024 * 1024 {
        return Err("snappy: implausible length".into());
    }
    let mut out: Vec<u8> = Vec::with_capacity(want);
    while pos < src.len() {
        let tag = src[pos];
        pos += 1;
        match tag & 3 {
            0 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 {
                    let extra = len - 59;
                    let b = take(src, &mut pos, extra)?;
                    len = b.iter().rev().fold(0usize, |a, &x| (a << 8) | x as usize);
                }
                out.extend_from_slice(take(src, &mut pos, len + 1)?);
            }
            kind => {
                let (len, offset) = match kind {
                    1 => {
                        let b = take(src, &mut pos, 1)?[0] as usize;
                        (4 + ((tag >> 2) & 7) as usize, (((tag >> 5) as usize) << 8) | b)
                    }
                    2 => {
                        let b = take(src, &mut pos, 2)?;
                        (1 + (tag >> 2) as usize, b[0] as usize | (b[1] as usize) << 8)
                    }
                    _ => {
                        let b = take(src, &mut pos, 4)?;
                        (1 + (tag >> 2) as usize, u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
                    }
                };
                if offset == 0 || offset > out.len() {
                    return Err("snappy: bad copy offset".into());
                }
                for _ in 0..len {
                    out.push(out[out.len() - offset]);
                }
            }
        }
        if out.len() > want {
            return Err("snappy: output exceeds declared length".into());
        }
    }
    if out.len() != want {
        return Err("snappy: length mismatch".into());
    }
    Ok(out)
}

fn put(latest: &mut Latest, key: &[u8], seq: u64, val: Option<Vec<u8>>) {
    match latest.get(key) {
        Some((s, _)) if *s >= seq => {}
        _ => {
            latest.insert(key.to_vec(), (seq, val));
        }
    }
}

// ───────────── write-ahead log ─────────────

fn read_log(data: &[u8], latest: &mut Latest) -> Result<(), String> {
    let mut batch: Vec<u8> = Vec::new();
    let mut pos = 0;
    let mut result = Ok(());
    'outer: while pos < data.len() {
        let block_end = ((pos / LOG_BLOCK) + 1) * LOG_BLOCK;
        if block_end - pos < 7 {
            pos = block_end;
            continue;
        }
        if pos + 7 > data.len() {
            break;
        }
        let len = u16::from_le_bytes([data[pos + 4], data[pos + 5]]) as usize;
        let typ = data[pos + 6];
        let start = pos + 7;
        if typ == 0 && len == 0 {
            pos = block_end; // zero padding
            continue;
        }
        if start + len > data.len() || start + len > block_end {
            result = Err("log: torn record at the tail".to_string());
            break;
        }
        let payload = &data[start..start + len];
        pos = start + len;
        match typ {
            1 => {
                batch.clear();
                batch.extend_from_slice(payload);
            }
            2 => {
                batch.clear();
                batch.extend_from_slice(payload);
                continue;
            }
            3 => {
                batch.extend_from_slice(payload);
                continue;
            }
            4 => batch.extend_from_slice(payload),
            _ => {
                result = Err(format!("log: unknown record type {}", typ));
                break 'outer;
            }
        }
        if let Err(e) = apply_batch(&batch, latest) {
            result = Err(e);
            break;
        }
    }
    result
}

fn apply_batch(b: &[u8], latest: &mut Latest) -> Result<(), String> {
    if b.len() < 12 {
        return Err("log: short batch".into());
    }
    let seq = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let count = u32::from_le_bytes(b[8..12].try_into().unwrap()) as u64;
    let mut pos = 12;
    for i in 0..count {
        let kind = *b.get(pos).ok_or("log: truncated batch")?;
        pos += 1;
        let klen = varint(b, &mut pos)? as usize;
        let key = take(b, &mut pos, klen)?;
        match kind {
            1 => {
                let vlen = varint(b, &mut pos)? as usize;
                let val = take(b, &mut pos, vlen)?.to_vec();
                put(latest, key, seq + i, Some(val));
            }
            0 => put(latest, key, seq + i, None),
            _ => return Err("log: bad batch tag".into()),
        }
    }
    Ok(())
}

// ───────────── sorted tables ─────────────

fn block_handle(buf: &[u8], pos: &mut usize) -> Result<(usize, usize), String> {
    Ok((varint(buf, pos)? as usize, varint(buf, pos)? as usize))
}

fn read_block(file: &[u8], off: usize, size: usize) -> Result<Vec<u8>, String> {
    let end = off.checked_add(size).filter(|e| e + 5 <= file.len()).ok_or("table: block out of range")?;
    let raw = &file[off..end];
    match file[end] {
        0 => Ok(raw.to_vec()),
        1 => snappy_decompress(raw),
        t => Err(format!("table: unsupported compression {}", t)),
    }
}

/// Iterate `(key, value)` of a block (prefix-compressed entries + restart array).
fn block_entries(block: &[u8]) -> Result<Pairs, String> {
    if block.len() < 4 {
        return Err("block too small".into());
    }
    let restarts = u32::from_le_bytes(block[block.len() - 4..].try_into().unwrap()) as usize;
    let data_end = block
        .len()
        .checked_sub(4 + restarts * 4)
        .ok_or("block: bad restart count")?;
    let (mut pos, mut key, mut out) = (0usize, Vec::<u8>::new(), Vec::new());
    while pos < data_end {
        let shared = varint(block, &mut pos)? as usize;
        let non_shared = varint(block, &mut pos)? as usize;
        let vlen = varint(block, &mut pos)? as usize;
        if shared > key.len() {
            return Err("block: bad shared prefix".into());
        }
        key.truncate(shared);
        key.extend_from_slice(take(block, &mut pos, non_shared)?);
        let val = take(block, &mut pos, vlen)?.to_vec();
        out.push((key.clone(), val));
    }
    Ok(out)
}

fn read_table(file: &[u8], latest: &mut Latest) -> Result<(), String> {
    if file.len() < 48 {
        return Err("table: too small".into());
    }
    let footer = &file[file.len() - 48..];
    if u64::from_le_bytes(footer[40..48].try_into().unwrap()) != TABLE_MAGIC {
        return Err("table: bad magic".into());
    }
    let mut pos = 0;
    let _metaindex = block_handle(footer, &mut pos)?;
    let (ioff, isize) = block_handle(footer, &mut pos)?;
    let index = block_entries(&read_block(file, ioff, isize)?)?;
    let mut first_error = None;
    for (_, handle) in index {
        let mut p = 0;
        let res = block_handle(&handle, &mut p)
            .and_then(|(off, size)| read_block(file, off, size))
            .and_then(|b| block_entries(&b));
        match res {
            Ok(entries) => {
                for (ikey, val) in entries {
                    if ikey.len() < 8 {
                        continue;
                    }
                    let (user, trailer) = ikey.split_at(ikey.len() - 8);
                    let tag = u64::from_le_bytes(trailer.try_into().unwrap());
                    let (seq, kind) = (tag >> 8, (tag & 0xff) as u8);
                    put(latest, user, seq, if kind == 1 { Some(val) } else { None });
                }
            }
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// A whole database's contents with the (non-fatal) warnings met while reading it.
pub type DbSnapshot = (HashMap<Vec<u8>, Vec<u8>>, Vec<String>);

/// Current contents of the database in `dir` (key → value), plus warnings about unreadable parts.
pub fn read_db(dir: &Path) -> Result<DbSnapshot, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("cannot open {}: {}", dir.display(), e))?;
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    files.sort();
    let mut latest: Latest = HashMap::new();
    let mut warnings = Vec::new();
    let mut seen_any = false;
    for f in files {
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !matches!(ext.as_str(), "log" | "ldb" | "sst") {
            continue;
        }
        let Ok(data) = fs::read(&f) else { continue }; // compacted away while we looked
        seen_any = true;
        let res = if ext == "log" { read_log(&data, &mut latest) } else { read_table(&data, &mut latest) };
        if let Err(e) = res {
            warnings.push(format!("{}: {}", f.file_name().unwrap_or_default().to_string_lossy(), e));
        }
    }
    if !seen_any {
        return Err("no LevelDB files found".into());
    }
    let out = latest.into_iter().filter_map(|(k, (_, v))| v.map(|v| (k, v))).collect();
    Ok((out, warnings))
}

#[cfg(test)]
pub mod builder {
    //! Minimal writers used by tests to fabricate databases in LevelDB's real on-disk formats.
    use super::*;

    pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// WAL file holding one batch per `(seq, ops)`; `None` value = delete. Records are split
    /// across 32 KiB blocks (FIRST/MIDDLE/LAST) exactly as LevelDB does.
    pub type Op<'a> = (&'a [u8], Option<&'a [u8]>);

    pub fn wal(batches: &[(u64, Vec<Op>)]) -> Vec<u8> {
        let mut file: Vec<u8> = Vec::new();
        for (seq, ops) in batches {
            let mut b = Vec::new();
            b.extend_from_slice(&seq.to_le_bytes());
            b.extend_from_slice(&(ops.len() as u32).to_le_bytes());
            for (k, v) in ops {
                match v {
                    Some(v) => {
                        b.push(1);
                        put_varint(&mut b, k.len() as u64);
                        b.extend_from_slice(k);
                        put_varint(&mut b, v.len() as u64);
                        b.extend_from_slice(v);
                    }
                    None => {
                        b.push(0);
                        put_varint(&mut b, k.len() as u64);
                        b.extend_from_slice(k);
                    }
                }
            }
            let mut rest = &b[..];
            let mut first = true;
            loop {
                let left = LOG_BLOCK - (file.len() % LOG_BLOCK);
                if left < 7 {
                    file.extend(std::iter::repeat_n(0u8, left));
                    continue;
                }
                let n = rest.len().min(left - 7);
                let last = n == rest.len();
                let typ = match (first, last) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, false) => 3,
                    (false, true) => 4,
                };
                file.extend_from_slice(&[0, 0, 0, 0]); // crc is not verified by the reader
                file.extend_from_slice(&(n as u16).to_le_bytes());
                file.push(typ);
                file.extend_from_slice(&rest[..n]);
                rest = &rest[n..];
                first = false;
                if last {
                    break;
                }
            }
        }
        file
    }

    fn block(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut b = Vec::new();
        for (k, v) in entries {
            put_varint(&mut b, 0);
            put_varint(&mut b, k.len() as u64);
            put_varint(&mut b, v.len() as u64);
            b.extend_from_slice(k);
            b.extend_from_slice(v);
        }
        b.extend_from_slice(&0u32.to_le_bytes()); // restart[0]
        b.extend_from_slice(&1u32.to_le_bytes());
        b
    }

    fn internal(k: &[u8], seq: u64, kind: u8) -> Vec<u8> {
        let mut v = k.to_vec();
        v.extend_from_slice(&((seq << 8) | kind as u64).to_le_bytes());
        v
    }

    /// Literal-only Snappy stream, with a back-reference appended when `repeat_tail` is set.
    pub fn snappy_literal(data: &[u8], repeat_tail: Option<(usize, usize)>) -> Vec<u8> {
        let mut out = Vec::new();
        let total = data.len() + repeat_tail.map(|(_, l)| l).unwrap_or(0);
        put_varint(&mut out, total as u64);
        // literal run (length encoded in the extra-bytes form to exercise it)
        out.push(61 << 2);
        out.extend_from_slice(&((data.len() - 1) as u16).to_le_bytes());
        out.extend_from_slice(data);
        if let Some((offset, len)) = repeat_tail {
            out.push(2 | (((len - 1) as u8) << 2)); // copy with 2-byte offset
            out.extend_from_slice(&(offset as u16).to_le_bytes());
        }
        out
    }

    /// Table with one data block per chunk of entries; `compress` stores blocks as Snappy.
    pub fn table(entries: &[(&[u8], u64, u8, &[u8])], per_block: usize, compress: bool) -> Vec<u8> {
        let mut file = Vec::new();
        let mut index: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for chunk in entries.chunks(per_block) {
            let es: Vec<(Vec<u8>, Vec<u8>)> =
                chunk.iter().map(|(k, s, t, v)| (internal(k, *s, *t), v.to_vec())).collect();
            let raw = block(&es);
            let (body, typ) = if compress { (snappy_literal(&raw, None), 1u8) } else { (raw, 0u8) };
            let off = file.len();
            file.extend_from_slice(&body);
            file.push(typ);
            file.extend_from_slice(&[0, 0, 0, 0]);
            let mut handle = Vec::new();
            put_varint(&mut handle, off as u64);
            put_varint(&mut handle, body.len() as u64);
            index.push((es.last().unwrap().0.clone(), handle));
        }
        let ib = block(&index);
        let ioff = file.len();
        file.extend_from_slice(&ib);
        file.push(0);
        file.extend_from_slice(&[0, 0, 0, 0]);
        let mut footer = Vec::new();
        put_varint(&mut footer, 0); // metaindex handle (unused by the reader)
        put_varint(&mut footer, 0);
        put_varint(&mut footer, ioff as u64);
        put_varint(&mut footer, ib.len() as u64);
        footer.resize(40, 0);
        footer.extend_from_slice(&TABLE_MAGIC.to_le_bytes());
        file.extend_from_slice(&footer);
        file
    }
}

#[cfg(test)]
mod tests {
    use super::builder::*;
    use super::*;
    use crate::fsutil::testutil::TempDir;

    #[test]
    fn snappy_literals_and_copies() {
        let lit = snappy_literal(b"hello world, hello", None);
        assert_eq!(snappy_decompress(&lit).unwrap(), b"hello world, hello");
        // copy 5 bytes from 12 back: "hello"
        let with_copy = snappy_literal(b"hello world, hello", Some((18, 5)));
        assert_eq!(snappy_decompress(&with_copy).unwrap(), b"hello world, hellohello");
        // overlapping copy (run-length style): "ab" then copy 6 from offset 2
        let mut s = Vec::new();
        put_varint(&mut s, 8);
        s.push(1 << 2);
        s.extend_from_slice(b"ab");
        s.push(2 | (5 << 2));
        s.extend_from_slice(&2u16.to_le_bytes());
        assert_eq!(snappy_decompress(&s).unwrap(), b"abababab");
        // 1-byte-offset copy form
        let mut s = Vec::new();
        put_varint(&mut s, 8);
        s.push(3 << 2);
        s.extend_from_slice(b"wxyz");
        s.push(1); // copy, 1-byte offset form: len 4, offset high bits 0
        s.push(4);
        assert_eq!(snappy_decompress(&s).unwrap(), b"wxyzwxyz");
        // corrupt inputs fail cleanly
        assert!(snappy_decompress(&[]).is_err());
        assert!(snappy_decompress(&[5, 0, b'a']).is_err());
        let mut bad = lit.clone();
        bad[0] = 99;
        assert!(snappy_decompress(&bad).is_err());
        let mut bad_off = Vec::new();
        put_varint(&mut bad_off, 4);
        bad_off.push(2 | (3 << 2));
        bad_off.extend_from_slice(&9u16.to_le_bytes());
        assert!(snappy_decompress(&bad_off).is_err());
    }

    #[test]
    fn newest_write_wins_and_deletes_remove_keys() {
        let d = TempDir::new("ldb1");
        // older table holds a, b, c; the log overwrites b, deletes c and adds d
        fs::write(
            d.join("000005.ldb"),
            table(&[(b"a", 1, 1, b"A1"), (b"b", 2, 1, b"B1"), (b"c", 3, 1, b"C1")], 2, true),
        )
        .unwrap();
        fs::write(
            d.join("000006.log"),
            wal(&[(10, vec![(b"b", Some(b"B2")), (b"c", None)]), (12, vec![(b"d", Some(b"D1"))])]),
        )
        .unwrap();
        let (db, warn) = read_db(d.path()).unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(db.get(&b"a"[..]).map(|v| &v[..]), Some(&b"A1"[..]));
        assert_eq!(db.get(&b"b"[..]).map(|v| &v[..]), Some(&b"B2"[..]));
        assert_eq!(db.get(&b"c"[..]), None);
        assert_eq!(db.get(&b"d"[..]).map(|v| &v[..]), Some(&b"D1"[..]));
        assert_eq!(db.len(), 3);
    }

    #[test]
    fn a_stale_table_entry_never_beats_a_newer_log_record() {
        let d = TempDir::new("ldb2");
        fs::write(d.join("1.ldb"), table(&[(b"k", 50, 1, b"new")], 8, false)).unwrap();
        fs::write(d.join("2.log"), wal(&[(7, vec![(b"k", Some(b"old"))])])).unwrap();
        let (db, _) = read_db(d.path()).unwrap();
        assert_eq!(&db[&b"k"[..]][..], b"new");
        // and a table-level tombstone newer than the log removes the key
        fs::write(d.join("1.ldb"), table(&[(b"k", 60, 0, b"")], 8, false)).unwrap();
        let (db, _) = read_db(d.path()).unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn log_records_spanning_blocks_are_reassembled() {
        let d = TempDir::new("ldb3");
        let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let small = vec![7u8; 20_000];
        fs::write(
            d.join("1.log"),
            wal(&[(1, vec![(b"big", Some(&big))]), (2, vec![(b"small", Some(&small))]), (3, vec![(b"end", Some(b"x"))])]),
        )
        .unwrap();
        let (db, warn) = read_db(d.path()).unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(db[&b"big"[..]], big);
        assert_eq!(db[&b"small"[..]], small);
        assert_eq!(&db[&b"end"[..]][..], b"x");
    }

    /// Foreign files are untrusted input: whatever the bytes, readers must return, never panic.
    #[test]
    fn random_and_mutated_inputs_never_panic() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let good_table = table(&[(b"a", 1, 1, b"AAAA"), (b"b", 2, 1, b"BBBB"), (b"c", 3, 0, b"")], 1, true);
        let good_log = wal(&[(1, vec![(b"k", Some(&[5u8; 300][..])), (b"z", None)]), (9, vec![(b"q", Some(b"v"))])]);
        let good_snappy = snappy_literal(b"hello world, hello", Some((18, 5)));
        for i in 0..4000 {
            let pick = |g: &Vec<u8>, rnd: &mut dyn FnMut() -> u64| -> Vec<u8> {
                let mut v = g.clone();
                match rnd() % 4 {
                    0 => v.truncate((rnd() as usize) % (v.len() + 1)),
                    1 => {
                        for _ in 0..1 + rnd() % 6 {
                            let n = v.len();
                            if n > 0 {
                                v[(rnd() as usize) % n] = rnd() as u8;
                            }
                        }
                    }
                    2 => v = (0..rnd() % 200).map(|_| rnd() as u8).collect(),
                    _ => {
                        let at = (rnd() as usize) % (v.len() + 1);
                        v.splice(at..at, (0..rnd() % 40).map(|_| rnd() as u8));
                    }
                }
                v
            };
            let mut latest = Latest::new();
            let _ = read_table(&pick(&good_table, &mut rnd), &mut latest);
            let _ = read_log(&pick(&good_log, &mut rnd), &mut latest);
            let _ = snappy_decompress(&pick(&good_snappy, &mut rnd));
            if i % 500 == 0 {
                let _ = snappy_decompress(&[0xff; 32]);
                let _ = read_table(&[0u8; 48], &mut latest);
            }
        }
    }

    #[test]
    fn damage_costs_only_what_it_touches() {
        let d = TempDir::new("ldb4");
        let mut log = wal(&[(1, vec![(b"keep", Some(b"1"))]), (2, vec![(b"lost", Some(&[9u8; 500]))])]);
        log.truncate(log.len() - 100); // torn tail, as when the writer is mid-append
        fs::write(d.join("1.log"), &log).unwrap();
        fs::write(d.join("2.ldb"), b"this is not a table at all, just garbage bytes of some length........").unwrap();
        fs::write(d.join("3.ldb"), table(&[(b"t", 5, 1, b"ok")], 4, true)).unwrap();
        fs::write(d.join("LOCK"), b"").unwrap();
        let (db, warn) = read_db(d.path()).unwrap();
        assert_eq!(&db[&b"keep"[..]][..], b"1");
        assert_eq!(&db[&b"t"[..]][..], b"ok");
        assert!(!db.contains_key(&b"lost"[..]));
        assert_eq!(warn.len(), 2, "{warn:?}");
        assert!(read_db(&d.join("missing")).is_err());
        let empty = TempDir::new("ldb5");
        assert!(read_db(empty.path()).is_err());
    }
}
