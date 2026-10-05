//! End-user guide content for the Studio's **Guide** screen.
//!
//! The pages are authored as plain task-oriented markdown under `docs/guide/`
//! (the single source of truth — also readable on GitHub / reusable for a docs
//! site). They are bundled into the WASM binary with `include_str!` and
//! rendered to HTML at runtime by [`render`]. No network fetch, no separate
//! deploy: the guide ships inside the Studio and works fully offline.
//!
//! Cross-page links are authored as ordinary **relative `.md` links** (e.g.
//! `[Teams](07-teams.md)`) so they resolve correctly when the files are read on
//! GitHub, *and* drive in-app navigation: the Guide panel intercepts clicks on
//! those links and maps the filename back to a page via [`index_for_href`].
//!
//! Adding a page = drop a `NN-name.md` in `docs/guide/` and add one [`Page`]
//! entry below (order here is the order in the screen's page list).

use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

/// One guide page: a stable `id`, the source `file` name (used to resolve
/// in-app `.md` cross-links), a short `title` for the page list, and the raw
/// markdown `body` bundled from `docs/guide/`.
pub struct Page {
    pub id: &'static str,
    pub file: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

/// The ordered guide. `include_str!` paths are relative to this file
/// (`crates/aivyx-web/src/guide.rs` → `../../../docs/guide/`).
pub const PAGES: &[Page] = &[
    Page {
        id: "welcome",
        file: "01-welcome.md",
        title: "Welcome",
        body: include_str!("../../../docs/guide/01-welcome.md"),
    },
    Page {
        id: "getting-started",
        file: "02-getting-started.md",
        title: "Getting started",
        body: include_str!("../../../docs/guide/02-getting-started.md"),
    },
    Page {
        id: "create-your-agent",
        file: "03-create-your-agent.md",
        title: "Create your agent",
        body: include_str!("../../../docs/guide/03-create-your-agent.md"),
    },
    Page {
        id: "chat-and-missions",
        file: "04-chat-and-missions.md",
        title: "Chat & missions",
        body: include_str!("../../../docs/guide/04-chat-and-missions.md"),
    },
    Page {
        id: "memory",
        file: "05-memory.md",
        title: "Memory",
        body: include_str!("../../../docs/guide/05-memory.md"),
    },
    Page {
        id: "skills-and-persona",
        file: "06-skills-and-persona.md",
        title: "Skills & personality",
        body: include_str!("../../../docs/guide/06-skills-and-persona.md"),
    },
    Page {
        id: "teams",
        file: "07-teams.md",
        title: "Teams",
        body: include_str!("../../../docs/guide/07-teams.md"),
    },
    Page {
        id: "access-and-settings",
        file: "08-access-and-settings.md",
        title: "Access & settings",
        body: include_str!("../../../docs/guide/08-access-and-settings.md"),
    },
    Page {
        id: "screens",
        file: "09-screens-reference.md",
        title: "Screens reference",
        body: include_str!("../../../docs/guide/09-screens-reference.md"),
    },
    Page {
        id: "models-and-routing",
        file: "12-models-and-routing.md",
        title: "Models & routing",
        body: include_str!("../../../docs/guide/12-models-and-routing.md"),
    },
    Page {
        id: "chat-apps-and-accounts",
        file: "13-chat-apps-and-accounts.md",
        title: "Chat apps & accounts",
        body: include_str!("../../../docs/guide/13-chat-apps-and-accounts.md"),
    },
    Page {
        id: "desktop-app",
        file: "11-desktop-app.md",
        title: "Desktop app",
        body: include_str!("../../../docs/guide/11-desktop-app.md"),
    },
    Page {
        id: "terminal-and-cli",
        file: "14-terminal-and-cli.md",
        title: "Terminal and command line",
        body: include_str!("../../../docs/guide/14-terminal-and-cli.md"),
    },
    Page {
        id: "autonomy-and-routines",
        file: "15-autonomy-and-routines.md",
        title: "Autonomy and routines",
        body: include_str!("../../../docs/guide/15-autonomy-and-routines.md"),
    },
    Page {
        id: "named-instances",
        file: "16-named-instances.md",
        title: "Several assistants",
        body: include_str!("../../../docs/guide/16-named-instances.md"),
    },
    Page {
        id: "security-and-privacy",
        file: "17-security-and-privacy.md",
        title: "Security and privacy",
        body: include_str!("../../../docs/guide/17-security-and-privacy.md"),
    },
    Page {
        id: "backups-upgrades-and-moving",
        file: "18-backups-upgrades-and-moving.md",
        title: "Backups, upgrades and moving",
        body: include_str!("../../../docs/guide/18-backups-upgrades-and-moving.md"),
    },
    Page {
        id: "troubleshooting",
        file: "10-troubleshooting.md",
        title: "Troubleshooting",
        body: include_str!("../../../docs/guide/10-troubleshooting.md"),
    },
];

/// Resolve an in-app cross-page link `href` to a [`PAGES`] index.
///
/// Pages link to each other with relative `.md` filenames (`07-teams.md`). The
/// rendered anchors keep that verbatim in their `href` attribute, so we match
/// on the bare filename — tolerating an optional `./` prefix and any `#anchor`
/// fragment. Returns `None` for external links (`http(s)://`, `mailto:`) and
/// anything not naming a known page, so those fall through to default handling.
pub fn index_for_href(href: &str) -> Option<usize> {
    let name = href.trim_start_matches("./");
    let name = name.split('#').next().unwrap_or(name);
    PAGES.iter().position(|p| p.file == name)
}

/// Render a guide page's markdown to an HTML string for injection via
/// `dangerous_inner_html`. The input is our own committed, trusted content
/// (never user input), so raw-HTML injection is safe here. Tables and
/// strikethrough are enabled to match the markdown the pages use.
pub fn render(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(markdown, options);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Render markdown that may contain content the operator didn't author
/// themselves — an agent's `fs.write`, or arbitrary content under the
/// Documents screen's "Files" filesystem root. Unlike [`render`], this
/// closes two injection paths `render`'s own doc comment says it depends
/// on trusted input to avoid:
///
/// 1. Raw HTML: `Event::Html`/`Event::InlineHtml` are re-emitted as
///    escaped visible text instead of executable markup.
/// 2. Unsafe link/image destination schemes: `pulldown_cmark::html::push_html`
///    percent-encodes a link/image `dest_url` but never validates its
///    *scheme* — `[x](javascript:...)`/`[x](data:...)` pass straight
///    through as a live, clickable `href`/`src` otherwise. Every
///    `Tag::Link`/`Tag::Image`'s `dest_url` is checked against an
///    allowlist (`http`, `https`, `mailto`, or no scheme at all —
///    relative paths and `#anchor`s) and rewritten to `#` when it isn't
///    one of those, before the event stream ever reaches `push_html`.
///
/// A fenced ` ```mermaid ` code block is special-cased to
/// `<pre class="mermaid">` — the markup mermaid.js's browser build
/// expects to find and typeset in place (see
/// `crates/aivyx-web/src/main.rs`'s `FileViewer`/mermaid loader).
pub fn render_untrusted_markdown(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(markdown, options);

    let mut events: Vec<Event> = Vec::new();
    let mut in_mermaid = false;
    let mut mermaid_src = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info)))
                if info.as_ref() == "mermaid" =>
            {
                in_mermaid = true;
                mermaid_src.clear();
            }
            Event::End(TagEnd::CodeBlock) if in_mermaid => {
                in_mermaid = false;
                let escaped = escape_html(&mermaid_src);
                events.push(Event::Html(CowStr::from(format!(
                    "<pre class=\"mermaid\">{escaped}</pre>"
                ))));
            }
            Event::Text(text) if in_mermaid => {
                mermaid_src.push_str(&text);
            }
            Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: safe_dest_url(&dest_url),
                    title,
                    id,
                }));
            }
            Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
                events.push(Event::Start(Tag::Image {
                    link_type,
                    dest_url: safe_dest_url(&dest_url),
                    title,
                    id,
                }));
            }
            Event::Html(raw) | Event::InlineHtml(raw) => {
                // Reduce each tag to its bare "<name>"/"</name>" skeleton —
                // dropping attributes — then re-emit as an ordinary Text
                // event. `html::push_html` HTML-escapes every `Event::Text`
                // itself (via `escape_html_body_text`) when it writes it
                // out, so we push the skeleton UNESCAPED here; escaping it
                // ourselves too would double-escape (`<` -> `&lt;` ->
                // `&amp;lt;`). Dropping attributes (rather than displaying
                // them verbatim as inert escaped text) means a payload like
                // `<a href="x" onclick="evil()">` can't leave its
                // `onclick="evil()"` substring sitting in the page's text
                // at all, on top of it never being live markup.
                events.push(Event::Text(CowStr::from(strip_tag_attrs(&raw))));
            }
            other => events.push(other),
        }
    }

    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

/// Neutralize a markdown link/image destination whose URL scheme isn't on
/// the safe allowlist (`http`, `https`, `mailto`, or no scheme at all —
/// a relative path or `#anchor`), returning `"#"` in that case so the
/// link/image survives (keeping its visible text) with a dead, inert
/// destination instead of a live `javascript:`/`data:`/`vbscript:`/`file:`/…
/// URL. `pulldown_cmark::html::push_html` percent-encodes whatever
/// `dest_url` it's given (`escape_href`) but never checks its scheme, so
/// this check has to happen before the event reaches `push_html`.
///
/// The scheme is everything before the first `:` that appears before any
/// `/`, `?`, or `#` — matching how browsers resolve a URL scheme. A
/// destination with no such `:` (a relative path, or a bare `#anchor`)
/// has no scheme and is always safe. The scheme comparison is
/// case-insensitive (`jAvAsCrIpT:` is exactly as live as `javascript:`).
fn safe_dest_url(dest_url: &str) -> CowStr<'static> {
    let scheme_end = dest_url
        .find([':', '/', '?', '#'])
        .filter(|&i| dest_url.as_bytes()[i] == b':');
    let is_safe = match scheme_end {
        None => true, // no scheme at all — relative path or #anchor
        Some(end) => {
            let scheme = &dest_url[..end];
            scheme.eq_ignore_ascii_case("http")
                || scheme.eq_ignore_ascii_case("https")
                || scheme.eq_ignore_ascii_case("mailto")
        }
    };
    if is_safe {
        CowStr::from(dest_url.to_string())
    } else {
        CowStr::from("#")
    }
}

/// Escape the two characters that can turn text into markup (`&`, `<`) —
/// used for a mermaid fence's source before it's wrapped, unescaped, in a
/// raw `<pre>` [`Event::Html`] block (so this is the only thing standing
/// between that source and the page). `>` is deliberately left alone: it
/// can't open a tag on its own, and mermaid's own arrow syntax (`-->`)
/// uses it constantly — escaping it would just make rendered diagram
/// source harder to read for no safety benefit.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Reduce a raw HTML fragment (a `pulldown_cmark::Event::Html` /
/// `Event::InlineHtml` payload — a single tag, a run of several, or free
/// text mixed with tags) to just its element skeleton: every `<tag ...>` /
/// `</tag>` keeps its angle brackets and bare name but loses every
/// attribute, and anything that isn't a recognizable element tag (a
/// comment, a doctype, a malformed fragment) is dropped entirely rather
/// than guessed at. Text outside any tag passes through untouched — the
/// caller pushes the result as an `Event::Text`, so `html::push_html`'s own
/// escaping (`<`, `>`, `&`) is what actually neutralizes it as markup; this
/// function's job is only to keep attribute values (`onclick="evil()"`)
/// from sitting in the page's visible text at all.
fn strip_tag_attrs(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(rel_end) = chars[i..].iter().position(|&c| c == '>') {
                let end = i + rel_end;
                let body: String = chars[i + 1..end].iter().collect();
                let closing = body.starts_with('/');
                let name_src = if closing { &body[1..] } else { body.as_str() };
                let name: String = name_src
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !name.is_empty() {
                    if closing {
                        out.push_str(&format!("</{name}>"));
                    } else {
                        out.push_str(&format!("<{name}>"));
                    }
                }
                // Comments (`<!--`), doctypes, CDATA, or anything else that
                // doesn't start with a tag name: contribute nothing rather
                // than emit a guess.
                i = end + 1;
                continue;
            }
            // No matching `>` in this fragment — treat the rest as plain
            // text; `push_html`'s escaping still neutralizes the stray `<`.
            out.push(chars[i]);
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// True for a path whose extension is `.md` or `.markdown` (case-sensitive
/// — matches how agents/operators actually name files in this workspace;
/// broaden to case-insensitive later if that proves too strict).
pub fn is_markdown_path(path: &str) -> bool {
    path.ends_with(".md") || path.ends_with(".markdown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guide_file_is_registered() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/guide");
        let registered: Vec<&str> = PAGES.iter().map(|p| p.file).collect();
        let mut missing: Vec<String> = std::fs::read_dir(dir)
            .expect("docs/guide exists")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".md") && !registered.contains(&name.as_str()))
            .collect();
        missing.sort();
        assert!(missing.is_empty(), "docs/guide pages missing from guide.rs PAGES: {missing:?}");
    }

    #[test]
    fn escapes_raw_html_instead_of_passing_it_through() {
        let out = render_untrusted_markdown("hello <script>alert(1)</script> world");
        assert!(!out.contains("<script>"));
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn inline_html_is_also_escaped() {
        let out = render_untrusted_markdown("click <a href=\"x\" onclick=\"evil()\">here</a>");
        assert!(!out.contains("onclick="));
    }

    #[test]
    fn mermaid_fence_becomes_a_mermaid_pre_block() {
        let out = render_untrusted_markdown("```mermaid\ngraph TD; A-->B;\n```");
        assert!(out.contains("<pre class=\"mermaid\">"));
        assert!(out.contains("graph TD; A-->B;"));
    }

    #[test]
    fn non_mermaid_fence_renders_as_an_ordinary_code_block() {
        let out = render_untrusted_markdown("```rust\nfn f() {}\n```");
        assert!(out.contains("<pre><code"));
        assert!(!out.contains("class=\"mermaid\""));
    }

    #[test]
    fn ordinary_markdown_renders_headings_and_tables() {
        let out = render_untrusted_markdown("# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(out.contains("<h1>Title</h1>"));
        assert!(out.contains("<table>"));
    }

    #[test]
    fn javascript_scheme_link_is_neutralized() {
        let out = render_untrusted_markdown("[click me](javascript:alert(document.domain))");
        assert!(!out.contains("href=\"javascript:"));
        assert!(out.contains("href=\"#\""));
        assert!(out.contains("click me"));
    }

    #[test]
    fn mixed_case_javascript_scheme_link_is_also_neutralized() {
        let out = render_untrusted_markdown("[click me](jAvAsCrIpT:alert(1))");
        assert!(!out.to_lowercase().contains("href=\"javascript:"));
        assert!(out.contains("href=\"#\""));
    }

    #[test]
    fn data_scheme_link_is_neutralized() {
        let out =
            render_untrusted_markdown("[click me](data:text/html,<script>alert(1)</script>)");
        assert!(!out.contains("href=\"data:"));
        assert!(out.contains("href=\"#\""));
    }

    #[test]
    fn https_link_still_renders_as_a_real_clickable_href() {
        let out = render_untrusted_markdown("[normal link](https://example.com)");
        assert!(out.contains("href=\"https://example.com\""));
    }

    #[test]
    fn relative_link_still_renders_as_a_real_href() {
        let out = render_untrusted_markdown("[relative link](../other.md)");
        assert!(out.contains("href=\"../other.md\""));
    }

    #[test]
    fn anchor_link_still_renders_as_a_real_href() {
        let out = render_untrusted_markdown("[anchor](#section)");
        assert!(out.contains("href=\"#section\""));
    }

    #[test]
    fn is_markdown_path_matches_md_and_markdown_extensions() {
        assert!(is_markdown_path("notes.md"));
        assert!(is_markdown_path("README.markdown"));
        assert!(!is_markdown_path("notes.txt"));
        assert!(!is_markdown_path("script.js"));
    }

    // --- strip_tag_attrs: direct unit tests -------------------------------
    //
    // These call `strip_tag_attrs` itself (not through
    // `render_untrusted_markdown`) so they exercise its parsing directly,
    // independent of the blanket-escape safety net `push_html` provides
    // downstream. Several of them pin *current, non-ideal* parsing
    // behavior rather than asserting a "correct" HTML parse — see each
    // comment for why that behavior is still safe once the caller pushes
    // the result as an `Event::Text` and `pulldown_cmark::html::push_html`
    // escapes it with `escape_html_body_text` (which — per
    // `pulldown-cmark-escape`'s `HTML_BODY_TEXT_ESCAPE_TABLE` — escapes
    // only `&`, `<`, `>` in body text; `"` and `'` pass through
    // unescaped, since they're only dangerous inside an attribute value,
    // which this text never is).

    #[test]
    fn strip_tag_attrs_leaves_a_bare_tag_with_no_attributes_alone() {
        assert_eq!(strip_tag_attrs("<script>"), "<script>");
    }

    #[test]
    fn strip_tag_attrs_drops_attributes_including_event_handlers() {
        // The case the review named directly.
        assert_eq!(strip_tag_attrs("<a href=\"x\" onclick=\"evil()\">"), "<a>");
    }

    #[test]
    fn strip_tag_attrs_current_behavior_for_a_gt_inside_a_quoted_attribute_value() {
        // `strip_tag_attrs` has no concept of quoted attribute values: it
        // treats the *first* `>` anywhere after `<` as the tag's end. So
        // for `<a title=">">`, the tag is (mis)parsed as ending right after
        // `title="` (the `>` that's really just part of the quoted value),
        // leaving the tag name `a` extracted correctly but everything after
        // — `">` — falls through as ordinary text appended after the `<a>`
        // skeleton, verbatim.
        //
        // This is pinned as *current* behavior, not "correct" HTML parsing.
        // It remains safe: the caller pushes the whole returned string as a
        // single `Event::Text`, and `push_html`'s body-text escaping turns
        // the leftover `<` markers (none survive here) and `>` into `&gt;`
        // — there is no way for this to become live markup. If this
        // assertion ever needs to change because the parsing was
        // deliberately improved, update it; if it changes because of an
        // accidental regression, the safety argument above is what to
        // re-check first.
        assert_eq!(strip_tag_attrs("<a title=\">\">"), "<a>\">");
    }

    #[test]
    fn strip_tag_attrs_pulls_the_name_out_of_multiple_concatenated_tags() {
        // Can `strip_tag_attrs` actually receive multiple tags in one
        // fragment from a real pulldown-cmark 0.12 event stream? For
        // `Event::InlineHtml`, no: `Parser::scan_inline_html`
        // (pulldown-cmark 0.12.2's `parse.rs`) scans exactly one tag,
        // comment, or processing instruction per call, so inline HTML is
        // always one tag per event.
        //
        // For `Event::Html` (HTML *blocks*), yes: `firstpass.rs`'s
        // `parse_html_block_type_6_or_7` walks the block line by line and
        // `append_html_line` appends one `ItemBody::Html` item per source
        // *line*, not per tag. A single line containing several tags —
        // e.g. `<div><span onclick="evil()">x</span></div>` as its own
        // paragraph, which qualifies as an HTML block because `div` is a
        // type-6 block tag — becomes one `Event::Html` carrying that whole
        // line. This test's input models that real case, not an invented
        // one.
        assert_eq!(
            strip_tag_attrs("<div><span onclick=\"evil()\">x</span></div>"),
            "<div><span>x</span></div>"
        );
    }

    #[test]
    fn strip_tag_attrs_current_behavior_for_an_unterminated_tag() {
        // No matching `>` in the fragment at all: `strip_tag_attrs` falls
        // back to copying the rest through as plain text, character by
        // character, rather than guessing where the tag would have ended.
        // The stray `<` survives into the returned string, but — same as
        // above — it's only ever pushed as `Event::Text`, so `push_html`
        // escapes it to `&lt;` rather than it opening real markup.
        assert_eq!(strip_tag_attrs("<script"), "<script");
    }

    #[test]
    fn strip_tag_attrs_drops_the_self_closing_slash() {
        // Current behavior: the tag-name scan stops at the first
        // non-alphanumeric/non-hyphen character, so the trailing `/` in a
        // self-closing tag like `<br/>` is not part of the extracted name
        // and is silently dropped — `<br/>` becomes `<br>`, not `<br/>`.
        // Harmless here (this is reduced to inert escaped text either way,
        // never live markup), but worth pinning explicitly since the task
        // review called it out by name.
        assert_eq!(strip_tag_attrs("<br/>"), "<br>");
    }
}
