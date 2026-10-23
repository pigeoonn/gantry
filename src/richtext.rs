//! Turns what mod authors write (Nexus BBCode, Thunderstore Markdown, plain readmes) into a small,
//! safe set of HTML tags for the mod page. Everything is escaped first and only the tags below are ever
//! produced, so a hostile description cannot add a script, an event handler or an arbitrary address:
//!
//! `p br strong em u s ul ol li h3 h4 h5 blockquote pre code hr a img`
//!
//! Links are written as `<a class="ext" data-url="...">` (the page opens them in the browser on click,
//! only for http and https) and images only when they come from a site in [`IMAGE_HOSTS`].

use regex::Regex;
use std::sync::LazyLock;

/// Sites whose pictures may be shown inside Gantry (the page's content-security policy lists the same ones).
pub const IMAGE_HOSTS: &[&str] = &[
    "staticdelivery.nexusmods.com",
    "images.nexusmods.com",
    "thunderstore.io",
    "gcdn.thunderstore.io",
    "cdn.thunderstore.io",
    "ccdn.thunderstore.io",
    "raw.githubusercontent.com",
    "user-images.githubusercontent.com",
    "private-user-images.githubusercontent.com",
    "github.com",
    "i.imgur.com",
    "images.steamusercontent.com",
    "steamuserimages-a.akamaihd.net",
    "image.modcdn.io",
    "thumb.modcdn.io",
    "media.modcdn.io",
];

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// An address that may be opened or shown: http(s), one line, no markup characters.
pub fn safe_url(u: &str) -> Option<String> {
    let u = u.trim();
    let ok = (u.starts_with("https://") || u.starts_with("http://")) && u.len() <= 600 && !u.chars().any(|c| c.is_whitespace() || c.is_control() || "<>\"'`".contains(c));
    ok.then(|| u.to_string())
}

fn host_of(u: &str) -> &str {
    let rest = u.split("://").nth(1).unwrap_or("");
    rest.split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("")
}

/// A picture address from a site Gantry shows pictures of (https only).
pub fn image_url(u: &str) -> Option<String> {
    let u = safe_url(u)?;
    let host = host_of(&u).to_lowercase();
    (u.starts_with("https://") && IMAGE_HOSTS.contains(&host.as_str())).then_some(u)
}

fn link(url: &str, inner_html: &str) -> String {
    match safe_url(url) {
        Some(u) => format!("<a class=\"ext\" data-url=\"{}\" title=\"{}\">{}</a>", esc(&u), esc(&u), inner_html),
        None => inner_html.to_string(),
    }
}

fn img(url: &str, alt: &str) -> String {
    match image_url(url) {
        Some(u) => format!("<img class=\"rt-img\" src=\"{}\" alt=\"{}\" loading=\"lazy\" referrerpolicy=\"no-referrer\">", esc(&u), esc(alt)),
        None => match safe_url(url) {
            Some(_) => link(url, &esc(if alt.is_empty() { "picture" } else { alt })),
            None => String::new(),
        },
    }
}

static BARE_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'\[\]()]+"#).expect("regex"));

/// Plain text to HTML: escaped, with bare web addresses made clickable.
fn text_inline(s: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for m in BARE_URL.find_iter(s) {
        let url = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
        out.push_str(&esc(&s[last..m.start()]));
        out.push_str(&link(url, &esc(url)));
        last = m.start() + url.len();
    }
    out.push_str(&esc(&s[last..]));
    out
}

static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)</?[a-zA-Z][^<>]*>").expect("regex"));
static BREAKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<\s*(?:br\s*/?|/p|/div|/h[1-6]|/tr)\s*>").expect("regex"));
static LI_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<\s*li\b[^>]*>").expect("regex"));

static NUMERIC_ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(?:[xX]([0-9a-fA-F]{1,6})|([0-9]{1,7}));").expect("regex"));

/// The characters HTML writes as `&name;` or `&#92;` / `&#x5c;`, as the characters themselves (control characters stay as they are).
pub fn decode_entities(s: &str) -> String {
    let numeric = NUMERIC_ENTITY.replace_all(s, |c: &regex::Captures| {
        let n = c.get(1).and_then(|h| u32::from_str_radix(h.as_str(), 16).ok()).or_else(|| c.get(2).and_then(|d| d.as_str().parse().ok()));
        match n.and_then(char::from_u32) {
            Some(ch) if !ch.is_control() => ch.to_string(),
            _ => c[0].to_string(),
        }
    });
    numeric
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&ndash;", "\u{2013}")
        .replace("&mdash;", "\u{2014}")
        .replace("&hellip;", "\u{2026}")
        .replace("&bull;", "\u{2022}")
        .replace("&copy;", "\u{a9}")
        .replace("&reg;", "\u{ae}")
        .replace("&trade;", "\u{2122}")
        .replace("&laquo;", "\u{ab}")
        .replace("&raquo;", "\u{bb}")
        .replace("&amp;", "&")
}

/// HTML a site sent inside a text field becomes plain text with line breaks (the tags are dropped).
fn strip_html(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let s = LI_OPEN.replace_all(&s, "\n- ");
    let s = BREAKS.replace_all(&s, "\n");
    decode_entities(&TAG.replace_all(&s, ""))
}

// ───────────────────────────── BBCode ─────────────────────────────

#[derive(Debug)]
enum Tok<'a> {
    Text(&'a str),
    Open(String, String),
    Close(String),
}

fn tokens(src: &str) -> Vec<Tok<'_>> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(/?)([A-Za-z*]{1,12})(?:=([^\]\n]{0,600}))?\]").expect("regex"));
    let mut out = Vec::new();
    let mut last = 0;
    for c in RE.captures_iter(src) {
        let m = c.get(0).expect("match");
        if m.start() > last {
            out.push(Tok::Text(&src[last..m.start()]));
        }
        let name = c[2].to_lowercase();
        if c[1].is_empty() {
            out.push(Tok::Open(name, c.get(3).map(|a| a.as_str().trim().trim_matches(['"', '\'']).to_string()).unwrap_or_default()));
        } else {
            out.push(Tok::Close(name));
        }
        last = m.end();
    }
    if last < src.len() {
        out.push(Tok::Text(&src[last..]));
    }
    out
}

fn text_block(s: &str) -> String {
    text_inline(s).replace('\n', "<br>")
}

/// Nexus descriptions: BBCode (and sometimes stray HTML), as safe HTML.
pub fn bbcode_html(src: &str) -> String {
    let src = strip_html(src);
    let toks = tokens(&src);
    // (tag name, closing html) of what is open
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut out = String::new();
    // [url=..] text [/url] and [img] url [/img] and [code] need their raw content
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Text(t) => out.push_str(&text_block(t)),
            Tok::Open(name, arg) => match name.as_str() {
                "b" | "strong" => {
                    out.push_str("<strong>");
                    stack.push((name.clone(), "</strong>".into()));
                }
                "i" | "em" => {
                    out.push_str("<em>");
                    stack.push((name.clone(), "</em>".into()));
                }
                "u" => {
                    out.push_str("<u>");
                    stack.push((name.clone(), "</u>".into()));
                }
                "s" | "strike" => {
                    out.push_str("<s>");
                    stack.push((name.clone(), "</s>".into()));
                }
                "heading" | "h1" | "h2" | "h3" => {
                    out.push_str("<h4>");
                    stack.push((name.clone(), "</h4>".into()));
                }
                "quote" => {
                    out.push_str("<blockquote>");
                    stack.push((name.clone(), "</blockquote>".into()));
                }
                "list" => {
                    let tag = if arg == "1" || arg.eq_ignore_ascii_case("a") { "ol" } else { "ul" };
                    out.push_str(&format!("<{tag}>"));
                    stack.push((name.clone(), format!("</{tag}>")));
                }
                "*" => {
                    if !stack.iter().any(|(n, _)| n == "list") {
                        out.push_str("• ");
                    } else {
                        // finish what the previous item left open, then start the next item
                        while matches!(stack.last(), Some((n, _)) if n != "list") {
                            let (_, close) = stack.pop().expect("open tag");
                            out.push_str(&close);
                        }
                        out.push_str("<li>");
                        stack.push(("li".into(), "</li>".into()));
                    }
                }
                "url" => {
                    // collect the text up to [/url]
                    let mut inner = String::new();
                    let mut j = i + 1;
                    let mut raw = String::new();
                    while j < toks.len() {
                        match &toks[j] {
                            Tok::Close(n) if n == "url" => break,
                            Tok::Text(t) => {
                                raw.push_str(t);
                                inner.push_str(&text_inline_plain(t));
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    let target = if arg.is_empty() { raw.trim().to_string() } else { arg.clone() };
                    out.push_str(&link(&target, &if inner.trim().is_empty() { esc(&target) } else { inner }));
                    i = j;
                }
                "img" => {
                    let mut raw = String::new();
                    let mut j = i + 1;
                    while j < toks.len() {
                        match &toks[j] {
                            Tok::Close(n) if n == "img" => break,
                            Tok::Text(t) => raw.push_str(t),
                            _ => {}
                        }
                        j += 1;
                    }
                    out.push_str(&img(raw.trim(), ""));
                    i = j;
                }
                "code" | "pre" => {
                    let mut raw = String::new();
                    let mut j = i + 1;
                    while j < toks.len() {
                        match &toks[j] {
                            Tok::Close(n) if n == "code" || n == "pre" => break,
                            Tok::Text(t) => raw.push_str(t),
                            _ => {}
                        }
                        j += 1;
                    }
                    out.push_str(&format!("<pre><code>{}</code></pre>", esc(raw.trim_matches('\n'))));
                    i = j;
                }
                "youtube" => {
                    let mut raw = String::new();
                    let mut j = i + 1;
                    while j < toks.len() {
                        match &toks[j] {
                            Tok::Close(n) if n == "youtube" => break,
                            Tok::Text(t) => raw.push_str(t),
                            _ => {}
                        }
                        j += 1;
                    }
                    let id = raw.trim();
                    let u = if id.starts_with("http") { id.to_string() } else { format!("https://www.youtube.com/watch?v={id}") };
                    out.push_str(&link(&u, "▶ video"));
                    i = j;
                }
                "line" | "hr" => out.push_str("<hr>"),
                // styling that does not change the meaning: keep the words
                "size" | "color" | "font" | "center" | "left" | "right" | "spoiler" | "indent" | "table" | "tr" | "td" | "th" => {
                    stack.push((name.clone(), String::new()));
                }
                // not a tag we know: it was text, show it as written
                _ => out.push_str(&esc(&format!("[{}{}]", name, if arg.is_empty() { String::new() } else { format!("={arg}") }))),
            },
            Tok::Close(name) => {
                // a closing tag nothing opened is ignored
                if let Some(pos) = stack.iter().rposition(|(n, _)| n == name) {
                    while stack.len() > pos {
                        let (_, close) = stack.pop().expect("open tag");
                        out.push_str(&close);
                    }
                }
            }
        }
        i += 1;
    }
    while let Some((_, close)) = stack.pop() {
        out.push_str(&close);
    }
    tidy(&out)
}

/// Text inside a link: escaped, never nested links.
fn text_inline_plain(s: &str) -> String {
    esc(s)
}

/// Remove the line breaks BBCode leaves around lists, and trim blank lines.
fn tidy(html: &str) -> String {
    static EMPTY_BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:<br>\s*){3,}").expect("regex"));
    let html = html.replace("<ul><br>", "<ul>").replace("<ol><br>", "<ol>").replace("</li><br>", "</li>").replace("<br></li>", "</li>").replace("<br></ul>", "</ul>").replace("<br></ol>", "</ol>");
    let s = EMPTY_BR.replace_all(&html, "<br><br>").to_string();
    s.trim_matches(|c: char| c.is_whitespace()).trim_start_matches("<br>").trim_end_matches("<br>").to_string()
}

// ───────────────────────────── Markdown ─────────────────────────────

fn md_inline(s: &str) -> String {
    static IMG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\(([^)\s]+)(?:\s+&quot;[^)]*&quot;|\s+[^)]*)?\)").expect("regex"));
    static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)\s]+)(?:\s+[^)]*)?\)").expect("regex"));
    static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").expect("regex"));
    static BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*\n]+)\*\*|__([^_\n]+)__").expect("regex"));
    static ITAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[^*\w])\*([^*\s][^*\n]*?)\*($|[^*\w])|(^|[^_\w])_([^_\s][^_\n]*?)_($|[^_\w])").expect("regex"));
    // pull out the parts that carry addresses or code first, leave placeholders, escape the rest
    let mut parts: Vec<String> = Vec::new();
    let mut hold = |h: String| -> String {
        parts.push(h);
        format!("\u{1}{}\u{2}", parts.len() - 1)
    };
    let mut t = s.to_string();
    t = CODE.replace_all(&t, |c: &regex::Captures| hold(format!("<code>{}</code>", esc(&c[1])))).to_string();
    t = IMG.replace_all(&t, |c: &regex::Captures| hold(img(&c[2], &decode_entities(&c[1])))).to_string();
    t = LINK.replace_all(&t, |c: &regex::Captures| hold(link(&decode_entities(&c[2]), &text_inline(&decode_entities(&c[1]))))).to_string();
    let mut h = text_inline(&t);
    h = BOLD.replace_all(&h, |c: &regex::Captures| format!("<strong>{}</strong>", c.get(1).or(c.get(2)).map(|m| m.as_str()).unwrap_or(""))).to_string();
    h = ITAL
        .replace_all(&h, |c: &regex::Captures| {
            let (pre, body, post) = if c.get(2).is_some() { (&c[1], &c[2], &c[3]) } else { (&c[4], &c[5], &c[6]) };
            format!("{pre}<em>{body}</em>{post}")
        })
        .to_string();
    for (i, p) in parts.iter().enumerate() {
        h = h.replace(&format!("\u{1}{i}\u{2}"), p);
    }
    h
}

/// Thunderstore READMEs: Markdown (with some HTML mixed in), as safe HTML.
pub fn markdown_html(src: &str) -> String {
    static HTML_IMG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<img\b[^>]*?\bsrc\s*=\s*["']([^"']+)["'][^>]*>"#).expect("regex"));
    static HTML_A: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<a\b[^>]*?\bhref\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a\s*>"#).expect("regex"));
    static HTML_H: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<h([1-6])[^>]*>(.*?)</h[1-6]\s*>").expect("regex"));
    static ORDERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\d{1,3}[.)]\s+(.*)$").expect("regex"));
    static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+(.*)$").expect("regex"));
    static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$").expect("regex"));
    // three or more of one of - * _ (spaces allowed) on a line of their own
    let is_rule = |l: &str| {
        let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
        t.len() >= 3 && ['-', '*', '_'].iter().any(|m| t.chars().all(|c| c == *m))
    };

    let mut s = src.replace("\r\n", "\n").replace('\r', "\n");
    s = HTML_IMG.replace_all(&s, |c: &regex::Captures| format!("![]({})", &c[1])).to_string();
    s = HTML_A.replace_all(&s, |c: &regex::Captures| format!("[{}]({})", TAG.replace_all(&c[2], ""), &c[1])).to_string();
    s = HTML_H.replace_all(&s, |c: &regex::Captures| format!("\n{} {}\n", "#".repeat(c[1].parse::<usize>().unwrap_or(3)), TAG.replace_all(&c[2], ""))).to_string();
    s = LI_OPEN.replace_all(&s, "\n- ").to_string();
    s = BREAKS.replace_all(&s, "\n").to_string();
    // what is left of HTML is dropped, entities are decoded before being escaped again
    let s = decode_entities(&TAG.replace_all(&s, ""));

    let mut out = String::new();
    let mut para: Vec<String> = Vec::new();
    let mut list: Option<&'static str> = None;
    let mut in_code = false;
    let mut code = String::new();
    let flush_para = |para: &mut Vec<String>, out: &mut String| {
        if !para.is_empty() {
            out.push_str(&format!("<p>{}</p>", para.iter().map(|l| md_inline(l)).collect::<Vec<_>>().join("<br>")));
            para.clear();
        }
    };
    let close_list = |list: &mut Option<&'static str>, out: &mut String| {
        if let Some(t) = list.take() {
            out.push_str(&format!("</{t}>"));
        }
    };
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                out.push_str(&format!("<pre><code>{}</code></pre>", esc(code.trim_end_matches('\n'))));
                code.clear();
            } else {
                flush_para(&mut para, &mut out);
                close_list(&mut list, &mut out);
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            flush_para(&mut para, &mut out);
            close_list(&mut list, &mut out);
            continue;
        }
        if let Some(c) = HEADING.captures(line) {
            flush_para(&mut para, &mut out);
            close_list(&mut list, &mut out);
            let level = match c[1].len() {
                1 | 2 => "h3",
                3 => "h4",
                _ => "h5",
            };
            out.push_str(&format!("<{level}>{}</{level}>", md_inline(&c[2])));
        } else if is_rule(line) {
            flush_para(&mut para, &mut out);
            close_list(&mut list, &mut out);
            out.push_str("<hr>");
        } else if let Some(c) = line.trim_start().strip_prefix('>') {
            flush_para(&mut para, &mut out);
            close_list(&mut list, &mut out);
            out.push_str(&format!("<blockquote>{}</blockquote>", md_inline(c.trim())));
        } else if let Some(c) = BULLET.captures(line).filter(|_| !is_rule(line)) {
            flush_para(&mut para, &mut out);
            if list != Some("ul") {
                close_list(&mut list, &mut out);
                out.push_str("<ul>");
                list = Some("ul");
            }
            out.push_str(&format!("<li>{}</li>", md_inline(&c[1])));
        } else if let Some(c) = ORDERED.captures(line) {
            flush_para(&mut para, &mut out);
            if list != Some("ol") {
                close_list(&mut list, &mut out);
                out.push_str("<ol>");
                list = Some("ol");
            }
            out.push_str(&format!("<li>{}</li>", md_inline(&c[1])));
        } else {
            close_list(&mut list, &mut out);
            para.push(line.trim().to_string());
        }
    }
    if in_code {
        out.push_str(&format!("<pre><code>{}</code></pre>", esc(&code)));
    }
    flush_para(&mut para, &mut out);
    close_list(&mut list, &mut out);
    out
}

/// Plain text (a readme.txt, a changelog) as paragraphs, with `-` / `*` lines as a list.
pub fn text_html(src: &str) -> String {
    let src = strip_html(src);
    let mut out = String::new();
    let mut in_list = false;
    let mut para: Vec<String> = Vec::new();
    let flush = |para: &mut Vec<String>, out: &mut String| {
        if !para.is_empty() {
            out.push_str(&format!("<p>{}</p>", para.iter().map(|l| text_inline(l)).collect::<Vec<_>>().join("<br>")));
            para.clear();
        }
    };
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut para, &mut out);
            if in_list {
                out.push_str("</ul>");
                in_list = false;
            }
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")) {
            flush(&mut para, &mut out);
            if !in_list {
                out.push_str("<ul>");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>", text_inline(item.trim())));
        } else {
            if in_list {
                out.push_str("</ul>");
                in_list = false;
            }
            para.push(t.to_string());
        }
    }
    flush(&mut para, &mut out);
    if in_list {
        out.push_str("</ul>");
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn numeric_and_named_entities_become_the_characters_they_stand_for() {
        assert_eq!(decode_entities("C:&#92;Users&#x5C;me &amp;#92; &copy; &hellip;"), "C:\\Users\\me &#92; \u{a9} \u{2026}");
        assert_eq!(decode_entities("&#0; &#1114112; &#xD800; &#10;"), "&#0; &#1114112; &#xD800; &#10;", "nothing invalid or unprintable is produced");
        // the page shows a backslash, not the code for one, and still escapes what could be markup
        let html = bbcode_html("Put it in C:&#92;Games &#60;b&#62;x&#60;/b&#62;");
        assert!(html.contains("C:\\Games") && !html.contains("&amp;#92;"), "{html}");
        assert!(!html.contains("<b>x</b>"), "{html}");
    }

    use super::*;

    /// Nothing but the allowed tags and attributes may ever come out.
    fn assert_safe(html: &str) {
        static T: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(/?)([a-zA-Z0-9]+)([^>]*)>").expect("regex"));
        let allowed = ["p", "br", "strong", "em", "u", "s", "ul", "ol", "li", "h3", "h4", "h5", "blockquote", "pre", "code", "hr", "a", "img"];
        for c in T.captures_iter(html) {
            assert!(allowed.contains(&c[2].to_lowercase().as_str()), "tag {} in {html}", &c[2]);
            let lower = c[3].to_lowercase();
            assert!(!lower.contains(" on") && !lower.contains("javascript:") && !lower.contains("style="), "attribute in {html}");
        }
        // every attribute value is quoted, so an injected quote would break the shape: check the shape
        assert!(!html.contains("\" on") && !html.contains("<script") && !html.contains("<iframe"), "{html}");
    }

    #[test]
    fn bbcode_becomes_a_few_safe_tags() {
        let h = bbcode_html("[b]Hello[/b] [i]there[/i]<br />[url=https://example.com/x]a link[/url] and https://example.org/page.");
        assert!(h.contains("<strong>Hello</strong>") && h.contains("<em>there</em>"), "{h}");
        assert!(h.contains("<a class=\"ext\" data-url=\"https://example.com/x\"") && h.contains(">a link</a>"), "{h}");
        assert!(h.contains("data-url=\"https://example.org/page\""), "{h}");
        assert_safe(&h);
    }

    #[test]
    fn bbcode_lists_and_unknown_tags() {
        let h = bbcode_html("[list][*]one[*]two [b]bold[/b][*]three[/list][size=5][color=red]big[/color][/size] [weird=1]x");
        assert_eq!(h.matches("<li>").count(), 3, "{h}");
        assert_eq!(h.matches("</li>").count(), 3, "{h}");
        assert!(h.contains("<ul>") && h.contains("</ul>") && h.contains("big"), "{h}");
        assert!(h.contains("[weird=1]"), "unknown tags stay visible: {h}");
        assert_safe(&h);
        let o = bbcode_html("[list=1][*]a[*]b[/list]");
        assert!(o.starts_with("<ol>") && o.ends_with("</ol>"), "{o}");
    }

    #[test]
    fn hostile_input_is_inert() {
        for evil in [
            "[url=javascript:alert(1)]x[/url]",
            "[url=\"https://a.com\" onmouseover=\"alert(1)\"]x[/url]",
            "[img]https://evil.example/track.png[/img]",
            "[img]javascript:alert(1)[/img]",
            "<script>alert(1)</script><img src=x onerror=alert(1)>",
            "[b]<b onclick=x>[/b]",
            "[url=https://a.com/\"><script>x</script>]y[/url]",
            "[code]<script>alert(1)</script>[/code]",
            "[quote][quote][quote]deep",
            "[*][*][/list][/list][/url][/b]",
        ] {
            let h = bbcode_html(evil);
            assert_safe(&h);
            assert!(!h.contains("javascript:") || !h.contains("href"), "{evil} -> {h}");
            assert!(!h.contains("evil.example") || h.contains("data-url"), "a picture from an unknown site is never loaded: {evil} -> {h}");
            let m = markdown_html(evil);
            assert_safe(&m);
            let t = text_html(evil);
            assert_safe(&t);
        }
        assert!(!bbcode_html("[img]https://evil.example/track.png[/img]").contains("<img"));
    }

    #[test]
    fn pictures_only_from_known_sites() {
        assert!(bbcode_html("[img]https://staticdelivery.nexusmods.com/mods/1/a.jpg[/img]").contains("<img class=\"rt-img\" src=\"https://staticdelivery.nexusmods.com/mods/1/a.jpg\""));
        assert!(!bbcode_html("[img]http://staticdelivery.nexusmods.com/a.jpg[/img]").contains("<img"), "https only");
        assert!(!bbcode_html("[img]https://staticdelivery.nexusmods.com.evil.com/a.jpg[/img]").contains("<img"), "the host must match exactly");
        assert!(!bbcode_html("[img]https://user@evil.com/a.jpg[/img]").contains("<img"));
        assert_eq!(image_url("https://gcdn.thunderstore.io/live/repository/icons/x.png").as_deref(), Some("https://gcdn.thunderstore.io/live/repository/icons/x.png"));
    }

    #[test]
    fn markdown_blocks_and_inline() {
        let h = markdown_html("# Title\n\nSome **bold** and *italic* and `code` text.\n\n- one\n- two\n\n1. first\n2. second\n\n```\nlet x = 1 < 2;\n```\n\n> quoted\n\n[site](https://example.com) ![pic](https://gcdn.thunderstore.io/x.png)\n---\n");
        assert!(h.contains("<h3>Title</h3>"), "{h}");
        assert!(h.contains("<strong>bold</strong>") && h.contains("<em>italic</em>") && h.contains("<code>code</code>"), "{h}");
        assert_eq!(h.matches("<li>").count(), 4, "{h}");
        assert!(h.contains("<ul>") && h.contains("<ol>") && h.contains("<blockquote>quoted</blockquote>"), "{h}");
        assert!(h.contains("<pre><code>let x = 1 &lt; 2;</code></pre>"), "{h}");
        assert!(h.contains("data-url=\"https://example.com\"") && h.contains("<img class=\"rt-img\" src=\"https://gcdn.thunderstore.io/x.png\""), "{h}");
        assert!(h.contains("<hr>"), "{h}");
        assert_safe(&h);
    }

    #[test]
    fn markdown_with_html_inside() {
        let h = markdown_html("<p align=\"center\"><img src=\"https://raw.githubusercontent.com/o/r/main/a.png\" width=\"300\"></p>\n<h2>Install</h2>\n<ul><li>Drop it in <b>plugins</b></li></ul>\n<a href=\"https://github.com/o/r\">repo</a><br>done");
        assert!(h.contains("<img class=\"rt-img\" src=\"https://raw.githubusercontent.com/o/r/main/a.png\""), "{h}");
        assert!(h.contains("<h3>Install</h3>") && h.contains("<li>Drop it in plugins</li>") && h.contains("data-url=\"https://github.com/o/r\""), "{h}");
        assert_safe(&h);
    }

    #[test]
    fn plain_text_keeps_paragraphs_and_lists() {
        let h = text_html("Changes\n\n- fixed the crash\n- new cars\n\nThanks https://example.com/x, bye");
        assert!(h.contains("<ul><li>fixed the crash</li><li>new cars</li></ul>"), "{h}");
        assert!(h.contains("data-url=\"https://example.com/x\"") && h.contains(", bye"), "{h}");
        assert!(text_html("<li>a</li><li>b</li>").contains("<li>a</li>"));
    }

    #[test]
    fn nonsense_never_panics() {
        for s in ["", "[", "]", "[[[[]]]]", "[url", "[url=]", "[/url]", "[*]", "![](", "[](", "**", "__", "`", "```", "<", ">", "\u{1}\u{2}", "[img][img][img]", "[list][list][*][/list]"] {
            let _ = (bbcode_html(s), markdown_html(s), text_html(s));
        }
        let long = "[b]".repeat(5000) + &"x".repeat(100_000);
        assert_safe(&bbcode_html(&long));
    }
}
