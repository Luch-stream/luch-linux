//! Just enough XML for UPnP: device descriptions and SOAP answers are small, flat and machine-written, so elements
//! are found by their local name (any namespace prefix) instead of building a tree.

/// The inner text of every `<name>` element (any prefix), in order. Self-closing ones give "".
pub fn all<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some((open_end, self_closing, prefix)) = find_open(xml, name, at) {
        if self_closing {
            out.push("");
            at = open_end;
            continue;
        }
        let close = match prefix {
            Some(p) => format!("</{p}:{name}>"),
            None => format!("</{name}>"),
        };
        let Some(end) = xml[open_end..].find(&close).map(|e| open_end + e) else {
            break;
        };
        out.push(&xml[open_end..end]);
        at = end + close.len();
    }
    out
}

/// The first `<name>`'s text, unescaped and trimmed.
pub fn text(xml: &str, name: &str) -> Option<String> {
    all(xml, name).first().map(|s| unescape(s.trim()))
}

/// The value of attribute `attr` on the first `<name ...>` tag.
pub fn attr(xml: &str, name: &str, attr: &str) -> Option<String> {
    let (end, _, _) = find_open(xml, name, 0)?;
    let tag = &xml[..end];
    let tag = &tag[tag.rfind('<')?..];
    let key = format!(" {attr}=");
    let i = tag.find(&key)? + key.len();
    let q = tag[i..].chars().next()?;
    let rest = &tag[i + 1..];
    Some(unescape(&rest[..rest.find(q)?]))
}

/// Finds the next opening tag `<name` or `<prefix:name` from `from`: where its `>` ends, whether it was `/>`, its
/// prefix.
fn find_open<'a>(xml: &'a str, name: &str, from: usize) -> Option<(usize, bool, Option<&'a str>)> {
    let mut at = from;
    loop {
        let lt = at + xml[at..].find('<')?;
        let rest = &xml[lt + 1..];
        let tag_end = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
        let qname = &rest[..tag_end];
        let (prefix, local) = match qname.split_once(':') {
            Some((p, l)) => (Some(p), l),
            None => (None, qname),
        };
        if local == name && !qname.starts_with(['/', '?', '!']) {
            let gt = lt + 1 + rest.find('>')?;
            return Some((gt + 1, xml.as_bytes()[gt - 1] == b'/', prefix));
        }
        at = lt + 1;
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_by_local_name() {
        let x = r#"<?xml version="1.0"?><s:Envelope xmlns:s="x"><s:Body><u:GetPositionInfoResponse>
            <Track>1</Track><RelTime>0:01:02</RelTime><TrackURI/><TrackMetaData>&lt;DIDL-Lite&gt;</TrackMetaData>
            </u:GetPositionInfoResponse></s:Body></s:Envelope>"#;
        assert_eq!(text(x, "RelTime").as_deref(), Some("0:01:02"));
        assert_eq!(text(x, "TrackURI").as_deref(), Some(""));
        assert_eq!(text(x, "TrackMetaData").as_deref(), Some("<DIDL-Lite>"));
        assert_eq!(text(x, "Missing"), None);
        assert_eq!(all(x, "Body").len(), 1);
        // "Track" must not match "TrackURI" or "TrackMetaData".
        assert_eq!(all(x, "Track"), vec!["1"]);
    }

    #[test]
    fn attributes_and_entities() {
        let x = r#"<res protocolInfo="http-get:*:video/mp4:*" size='12'>u</res>"#;
        assert_eq!(
            attr(x, "res", "protocolInfo").as_deref(),
            Some("http-get:*:video/mp4:*")
        );
        assert_eq!(attr(x, "res", "size").as_deref(), Some("12"));
        assert_eq!(
            unescape("a &amp; b &#x41;&#66; &bogus; &"),
            "a & b AB &bogus; &"
        );
        assert_eq!(
            escape("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
        assert_eq!(unescape(&escape("Tom & Jerry <1>")), "Tom & Jerry <1>");
    }
}
