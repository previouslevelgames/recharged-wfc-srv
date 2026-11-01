//! HTML for the server's own pages.

/// Escapes text for HTML content and quoted attributes.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// `SERVER_LABEL`'s longest value, in characters.
const MAX_LABEL: usize = 16;
/// The tab icon's colour, `#` URL-encoded: the server's blue, or the label's
/// orange.
const ICON_FILL: &str = "%232a78d6";
const LABEL_ICON_FILL: &str = "%23c2410c";

/// Reads `SERVER_LABEL` with `var`: a short name such as `DEV` that the
/// server's pages show in their title, header and tab icon, to tell a test
/// server from the live one. `Ok(None)` when it is unset or empty.
pub fn server_label(var: impl Fn(&str) -> Option<String>) -> Result<Option<String>, String> {
    let Some(label) = var("SERVER_LABEL")
        .map(|label| label.trim().to_owned())
        .filter(|label| !label.is_empty())
    else {
        return Ok(None);
    };
    let allowed = |c: char| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.');
    if label.chars().count() > MAX_LABEL || !label.chars().all(allowed) {
        return Err(format!(
            "SERVER_LABEL: at most {MAX_LABEL} letters, digits, spaces, - _ or ."
        ));
    }
    Ok(Some(label))
}

/// Fills a page's label places: `{{label_title}}` before the title, `{{label}}`
/// for the badge and `{{icon_fill}}` in the tab icon. Fill them before the
/// page's other places, which may hold anyone's text.
pub fn label_page(page: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => page
            .replace("{{label_title}}", &format!("{} · ", escape(label)))
            .replace(
                "{{label}}",
                &format!(
                    r#"<span class="server-label" id="server-label">{}</span>"#,
                    escape(label)
                ),
            )
            .replace("{{icon_fill}}", LABEL_ICON_FILL),
        None => page
            .replace("{{label_title}}", "")
            .replace("{{label}}", "")
            .replace("{{icon_fill}}", ICON_FILL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_neutralizes_markup() {
        assert_eq!(
            escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }

    fn label(value: Option<&str>) -> Result<Option<String>, String> {
        server_label(|name| {
            assert_eq!(name, "SERVER_LABEL");
            value.map(str::to_owned)
        })
    }

    #[test]
    fn a_label_is_short_and_plain() {
        assert_eq!(label(None), Ok(None));
        assert_eq!(label(Some("  ")), Ok(None));
        assert_eq!(label(Some(" DEV ")), Ok(Some("DEV".into())));
        assert_eq!(
            label(Some("Test server-2")),
            Ok(Some("Test server-2".into()))
        );
        assert!(label(Some("<b>DEV</b>")).is_err());
        assert!(label(Some("{{content}}")).is_err());
        assert!(label(Some("A label far too long")).is_err());
    }

    #[test]
    fn a_labelled_page_shows_its_label() {
        let page = "<title>{{label_title}}Admin</title><link fill='{{icon_fill}}'>{{label}}";
        assert_eq!(
            label_page(page, None),
            "<title>Admin</title><link fill='%232a78d6'>"
        );
        assert_eq!(
            label_page(page, Some("DEV")),
            "<title>DEV · Admin</title><link fill='%23c2410c'>\
             <span class=\"server-label\" id=\"server-label\">DEV</span>"
        );
    }
}
