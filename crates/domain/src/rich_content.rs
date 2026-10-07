//! A bounded, strictly typed subset of the ProseMirror/Tiptap document shape.
//! These types are storage/export adapters, not an editor or an authorization grant.
use crate::AppError;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub(crate) const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 24;
pub(crate) const MAX_TEXT_BYTES: usize = 1_000_000;
const MAX_TABLE_ROWS: usize = 100;
const MAX_TABLE_COLUMNS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichContent {
    pub version: u8,
    pub node: RichNode,
}

/// Node names and `content` arrays follow Tiptap JSON; top-level evidence belongs
/// to `ContentBlock`, and must never be imported from pasted HTML node attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RichNode {
    #[serde(rename = "paragraph")]
    Paragraph {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<RichNode>,
    },
    #[serde(rename = "heading")]
    Heading {
        attrs: HeadingAttrs,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<RichNode>,
    },
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        marks: Vec<RichMark>,
    },
    #[serde(rename = "hardBreak")]
    HardBreak {},
    #[serde(rename = "bulletList")]
    BulletList { content: Vec<RichNode> },
    #[serde(rename = "orderedList")]
    OrderedList {
        attrs: OrderedListAttrs,
        content: Vec<RichNode>,
    },
    #[serde(rename = "listItem")]
    ListItem { content: Vec<RichNode> },
    #[serde(rename = "codeBlock")]
    CodeBlock {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attrs: Option<CodeBlockAttrs>,
        content: Vec<RichNode>,
    },
    #[serde(rename = "table")]
    Table { content: Vec<RichNode> },
    #[serde(rename = "tableRow")]
    TableRow { content: Vec<RichNode> },
    #[serde(rename = "tableHeader")]
    TableHeader {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attrs: Option<TableCellAttrs>,
        content: Vec<RichNode>,
    },
    #[serde(rename = "tableCell")]
    TableCell {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attrs: Option<TableCellAttrs>,
        content: Vec<RichNode>,
    },
    #[serde(rename = "media")]
    Media { attrs: MediaReference },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadingAttrs {
    pub level: u8,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderedListAttrs {
    pub start: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeBlockAttrs {
    pub language: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableCellAttrs {
    #[serde(default = "one")]
    pub colspan: u16,
    #[serde(default = "one")]
    pub rowspan: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colwidth: Option<Vec<u16>>,
    #[serde(rename = "textAlign", default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<CellAlignment>,
}
fn one() -> u16 {
    1
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellAlignment {
    Left,
    Center,
    Right,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaReference {
    pub object_id: Uuid,
    pub object_version: i64,
    pub sha256: String,
    pub alt: String,
    pub caption: String,
}
impl MediaReference {
    fn relative_path(&self) -> String {
        format!("media/{}-{}", self.object_id, self.object_version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum RichMark {
    #[serde(rename = "bold")]
    Bold,
    #[serde(rename = "italic")]
    Italic,
    #[serde(rename = "strike")]
    Strike,
    #[serde(rename = "underline")]
    Underline,
    #[serde(rename = "code")]
    Code,
    #[serde(rename = "link")]
    Link { attrs: LinkAttrs },
}
impl<'de> Deserialize<'de> for RichMark {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = serde_json::Value::deserialize(deserializer)?;
        let map = raw
            .as_object()
            .ok_or_else(|| Error::custom("rich mark must be an object"))?;
        let kind = map
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::custom("rich mark type required"))?;
        match kind {
            "bold" | "italic" | "strike" | "underline" | "code" if map.len() == 1 => {
                Ok(match kind {
                    "bold" => Self::Bold,
                    "italic" => Self::Italic,
                    "strike" => Self::Strike,
                    "underline" => Self::Underline,
                    _ => Self::Code,
                })
            }
            "link" if map.len() == 2 => {
                let attrs: LinkAttrs = serde_json::from_value(
                    map.get("attrs")
                        .cloned()
                        .ok_or_else(|| Error::custom("link attributes required"))?,
                )
                .map_err(Error::custom)?;
                Ok(Self::Link { attrs })
            }
            _ => Err(Error::custom("unknown rich mark or mark attribute")),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkAttrs {
    pub href: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Default)]
struct Limits {
    nodes: usize,
    text_bytes: usize,
}
impl Limits {
    fn text(&mut self, value: &str) -> Result<(), AppError> {
        self.text_bytes = self.text_bytes.saturating_add(value.len());
        if self.text_bytes > MAX_TEXT_BYTES {
            return Err(invalid("rich text limit exceeded"));
        }
        Ok(())
    }
}
fn invalid(message: &str) -> AppError {
    AppError::invalid_request(message)
}
fn safe_link(href: &str) -> bool {
    if href.is_empty() || href.len() > 2048 || href.contains('\\') {
        return false;
    }
    if href.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    if let Ok(url) = url::Url::parse(href) {
        return match url.scheme() {
            "https" | "http" => {
                url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
            }
            "mailto" => !url.path().is_empty(),
            _ => false,
        };
    }
    !href.starts_with("//")
        && url::Url::parse("https://content.invalid/")
            .ok()
            .and_then(|base| base.join(href).ok())
            .is_some_and(|url| url.scheme() == "https" && url.host_str() == Some("content.invalid"))
}
impl RichContent {
    /// Repair can delete unsupported child subtrees and replace text without
    /// moving survivors, changing surviving attrs/marks or introducing nodes.
    pub fn preserves_survivor_structure(&self, next: &Self) -> bool {
        self.version == next.version && self.node.preserves_survivor_structure(&next.node)
    }
    pub fn validate(&self) -> Result<(), AppError> {
        if self.version != 1 {
            return Err(invalid("unsupported rich node version"));
        }
        if !matches!(
            self.node,
            RichNode::Paragraph { .. }
                | RichNode::Heading { .. }
                | RichNode::BulletList { .. }
                | RichNode::OrderedList { .. }
                | RichNode::CodeBlock { .. }
                | RichNode::Table { .. }
                | RichNode::Media { .. }
        ) {
            return Err(invalid("rich block root must be a block node"));
        }
        let mut limits = Limits::default();
        self.node.validate(0, &mut limits)
    }
    pub fn media_references(&self) -> Vec<&MediaReference> {
        let mut result = Vec::new();
        self.node.gather_media(&mut result);
        result
    }
    pub(crate) fn complexity(&self) -> (usize, usize) {
        fn visit(node: &RichNode, nodes: &mut usize, bytes: &mut usize) {
            *nodes = nodes.saturating_add(1);
            match node {
                RichNode::Text { text, .. } => *bytes = bytes.saturating_add(text.len()),
                RichNode::Media { attrs } => {
                    *bytes = bytes
                        .saturating_add(attrs.alt.len())
                        .saturating_add(attrs.caption.len());
                }
                _ => {}
            }
            for child in node.children() {
                visit(child, nodes, bytes);
            }
        }
        let (mut nodes, mut bytes) = (0, 0);
        visit(&self.node, &mut nodes, &mut bytes);
        (nodes, bytes)
    }
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        self.node.gather_text(&mut text);
        text
    }
    pub fn markdown(&self) -> String {
        format!("{}\n\n", self.node.markdown())
    }
    pub fn html(&self) -> String {
        self.node.html()
    }
}
impl RichNode {
    fn preserves_survivor_structure(&self, next: &Self) -> bool {
        let attrs_match = match (self, next) {
            (Self::Paragraph { .. }, Self::Paragraph { .. })
            | (Self::HardBreak {}, Self::HardBreak {})
            | (Self::BulletList { .. }, Self::BulletList { .. })
            | (Self::ListItem { .. }, Self::ListItem { .. })
            | (Self::Table { .. }, Self::Table { .. })
            | (Self::TableRow { .. }, Self::TableRow { .. }) => true,
            (Self::Heading { attrs: a, .. }, Self::Heading { attrs: b, .. }) => a == b,
            (Self::OrderedList { attrs: a, .. }, Self::OrderedList { attrs: b, .. }) => a == b,
            (Self::CodeBlock { attrs: a, .. }, Self::CodeBlock { attrs: b, .. }) => a == b,
            (Self::TableHeader { attrs: a, .. }, Self::TableHeader { attrs: b, .. })
            | (Self::TableCell { attrs: a, .. }, Self::TableCell { attrs: b, .. }) => a == b,
            (Self::Text { marks: a, .. }, Self::Text { marks: b, .. }) => a == b,
            (Self::Media { attrs: a }, Self::Media { attrs: b }) => a == b,
            _ => false,
        };
        if !attrs_match {
            return false;
        }
        let mut survivors = self.children().iter();
        next.children().iter().all(|next_child| {
            survivors
                .by_ref()
                .any(|old_child| old_child.preserves_survivor_structure(next_child))
        })
    }
    fn validate(&self, depth: usize, limits: &mut Limits) -> Result<(), AppError> {
        limits.nodes += 1;
        if depth > MAX_DEPTH || limits.nodes > MAX_NODES {
            return Err(invalid("rich node depth or count limit exceeded"));
        }
        let (children, child_ok): (&[RichNode], fn(&RichNode) -> bool) = match self {
            Self::Paragraph { content } | Self::Heading { content, .. } => (content, |c| {
                matches!(c, Self::Text { .. } | Self::HardBreak {})
            }),
            Self::BulletList { content } | Self::OrderedList { content, .. } => {
                (content, |c| matches!(c, Self::ListItem { .. }))
            }
            Self::ListItem { content } => (content, |c| {
                matches!(
                    c,
                    Self::Paragraph { .. } | Self::BulletList { .. } | Self::OrderedList { .. }
                )
            }),
            Self::CodeBlock { content, .. } => (
                content,
                |c| matches!(c, Self::Text { marks, .. } if marks.is_empty()),
            ),
            Self::Table { content } => (content, |c| matches!(c, Self::TableRow { .. })),
            Self::TableRow { content } => (content, |c| {
                matches!(c, Self::TableHeader { .. } | Self::TableCell { .. })
            }),
            Self::TableHeader { content, .. } | Self::TableCell { content, .. } => {
                (content, |c| matches!(c, Self::Paragraph { .. }))
            }
            Self::Text { text, marks } => {
                if text.is_empty() {
                    return Err(invalid("empty rich text node"));
                }
                limits.text(text)?;
                let mut seen = std::collections::HashSet::new();
                for mark in marks {
                    let kind = std::mem::discriminant(mark);
                    if !seen.insert(kind) {
                        return Err(invalid("duplicate rich mark"));
                    }
                    if let RichMark::Link { attrs } = mark {
                        if !safe_link(&attrs.href)
                            || attrs.title.as_ref().is_some_and(|title| {
                                title.len() > 1024 || title.chars().any(char::is_control)
                            })
                        {
                            return Err(invalid("unsafe rich link"));
                        }
                        if let Some(title) = &attrs.title {
                            limits.text(title)?;
                        }
                    }
                }
                if marks.iter().any(|m| matches!(m, RichMark::Code)) && marks.len() > 1 {
                    return Err(invalid("inline code cannot combine with other marks"));
                }
                return Ok(());
            }
            Self::HardBreak {} => return Ok(()),
            Self::Media { attrs } => {
                if attrs.object_id.is_nil()
                    || attrs.object_version < 1
                    || attrs.sha256.len() != 64
                    || !attrs.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(invalid("invalid media object identity or digest"));
                }
                limits.text(&attrs.alt)?;
                limits.text(&attrs.caption)?;
                if attrs.alt.trim().is_empty() {
                    return Err(invalid("media alternative text required"));
                }
                return Ok(());
            }
        };
        match self {
            Self::Heading { attrs, .. } if !(1..=6).contains(&attrs.level) => {
                return Err(invalid("invalid heading level"));
            }
            Self::OrderedList { attrs, .. } if attrs.start == 0 => {
                return Err(invalid("ordered list start must be positive"));
            }
            Self::CodeBlock {
                attrs:
                    Some(CodeBlockAttrs {
                        language: Some(language),
                    }),
                ..
            } if language.len() > 64
                || !language.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'#')
                }) =>
            {
                return Err(invalid("invalid code language"));
            }
            Self::TableHeader { attrs, .. } | Self::TableCell { attrs, .. } => {
                if let Some(a) = attrs
                    && (a.colspan == 0
                        || a.rowspan == 0
                        || a.colspan > MAX_TABLE_COLUMNS as u16
                        || a.rowspan > MAX_TABLE_ROWS as u16
                        || a.colwidth
                            .as_ref()
                            .is_some_and(|w| w.len() != a.colspan as usize || w.contains(&0)))
                {
                    return Err(invalid("invalid table cell span or width"));
                }
            }
            _ => {}
        }
        if children.is_empty() && !matches!(self, Self::Paragraph { .. } | Self::CodeBlock { .. }) {
            return Err(invalid("rich container must not be empty"));
        }
        if matches!(self, Self::ListItem { content } if !matches!(content.first(), Some(Self::Paragraph { .. })))
        {
            return Err(invalid("list item must begin with a paragraph"));
        }
        for child in children {
            if !child_ok(child) {
                return Err(invalid("invalid rich node child"));
            }
            child.validate(depth + 1, limits)?;
        }
        if let Self::Table { content } = self {
            validate_table(content)?;
        }
        Ok(())
    }
    fn gather_media<'a>(&'a self, result: &mut Vec<&'a MediaReference>) {
        if let Self::Media { attrs } = self {
            result.push(attrs);
        }
        for child in self.children() {
            child.gather_media(result);
        }
    }
    fn gather_text(&self, text: &mut String) {
        match self {
            Self::Text { text: value, .. } => text.push_str(value),
            Self::HardBreak {} => text.push('\n'),
            Self::Media { attrs } => {
                text.push_str(&attrs.alt);
                text.push('\n');
                text.push_str(&attrs.caption);
            }
            _ => {
                for child in self.children() {
                    child.gather_text(text);
                }
                text.push('\n');
            }
        }
    }
    fn children(&self) -> &[RichNode] {
        match self {
            Self::Paragraph { content }
            | Self::Heading { content, .. }
            | Self::BulletList { content }
            | Self::OrderedList { content, .. }
            | Self::ListItem { content }
            | Self::CodeBlock { content, .. }
            | Self::Table { content }
            | Self::TableRow { content }
            | Self::TableHeader { content, .. }
            | Self::TableCell { content, .. } => content,
            _ => &[],
        }
    }
    fn markdown(&self) -> String {
        let inline = || inline_markdown(self.children());
        match self {
            Self::Text { text, marks } => {
                if !marks.is_empty()
                    && (text.trim() != text
                        || marks
                            .iter()
                            .any(|mark| matches!(mark, RichMark::Underline | RichMark::Code)))
                {
                    return self.html();
                }
                let mut value = escape_markdown(text);
                for mark in marks {
                    value = match mark {
                        RichMark::Bold => format!("**{value}**"),
                        RichMark::Italic => format!("*{value}*"),
                        RichMark::Strike => format!("~~{value}~~"),
                        RichMark::Underline => {
                            unreachable!("underlined text uses HTML to preserve all marks")
                        }
                        RichMark::Code => value,
                        RichMark::Link { attrs } => {
                            let target = attrs
                                .href
                                .replace('"', "%22")
                                .replace('<', "%3C")
                                .replace('>', "%3E")
                                .replace('(', "%28")
                                .replace(')', "%29");
                            match &attrs.title {
                                Some(title) => format!(
                                    "[{value}]({target} \"{}\")",
                                    title.replace('\\', "\\\\").replace('"', "\\\"")
                                ),
                                None => format!("[{value}]({target})"),
                            }
                        }
                    };
                }
                value
            }
            Self::HardBreak {} => "  \n".into(),
            Self::Paragraph { .. } => inline(),
            Self::Heading { attrs, .. } => {
                format!("{} {}", "#".repeat(attrs.level as usize), inline())
            }
            Self::BulletList { content } => list_markdown(content, None),
            Self::OrderedList { attrs, content }
                if u64::from(attrs.start) + content.len() as u64 > 1_000_000_000 =>
            {
                // CommonMark ordered markers have at most nine digits.
                self.html()
            }
            Self::OrderedList { attrs, content } => list_markdown(content, Some(attrs.start)),
            Self::ListItem { content } => {
                let mut output = String::new();
                for (index, child) in content.iter().enumerate() {
                    if index != 0 {
                        output.push_str(if matches!(child, Self::Paragraph { .. }) {
                            "\n\n"
                        } else {
                            "\n"
                        });
                    }
                    output.push_str(&child.markdown());
                }
                output
            }
            Self::CodeBlock { attrs, content } => {
                let code = content
                    .iter()
                    .filter_map(|c| {
                        if let Self::Text { text, .. } = c {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<String>();
                let fence = "`"
                    .repeat(3.max(code.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1));
                format!(
                    "{fence}{}\n{code}\n{fence}",
                    attrs
                        .as_ref()
                        .and_then(|a| a.language.as_deref())
                        .unwrap_or("")
                )
            }
            Self::Table { .. } => self.html(), // HTML preserves colspan/rowspan, alignment, and colwidth.
            Self::TableRow { .. } | Self::TableHeader { .. } | Self::TableCell { .. } => {
                self.html()
            }
            Self::Media { attrs } => format!(
                "![{}]({})\n\n{}",
                escape_markdown(&attrs.alt),
                attrs.relative_path(),
                escape_markdown(&attrs.caption)
            ),
        }
    }
    fn html(&self) -> String {
        let inline = || {
            self.children()
                .iter()
                .map(RichNode::html)
                .collect::<String>()
        };
        match self {
            Self::Text { text, marks } => {
                let mut value = escape_html(text);
                for mark in marks {
                    value = match mark {
                        RichMark::Bold => format!("<strong>{value}</strong>"),
                        RichMark::Italic => format!("<em>{value}</em>"),
                        RichMark::Strike => format!("<s>{value}</s>"),
                        RichMark::Underline => format!("<u>{value}</u>"),
                        RichMark::Code => format!("<code>{value}</code>"),
                        RichMark::Link { attrs } => format!(
                            "<a href=\"{}\"{} rel=\"noopener noreferrer\">{value}</a>",
                            escape_html(&attrs.href),
                            attrs.title.as_ref().map_or(String::new(), |title| format!(
                                " title=\"{}\"",
                                escape_html(title)
                            ))
                        ),
                    };
                }
                value
            }
            Self::HardBreak {} => "<br>".into(),
            Self::Paragraph { .. } => format!("<p>{}</p>", inline()),
            Self::Heading { attrs, .. } => format!("<h{0}>{1}</h{0}>", attrs.level, inline()),
            Self::BulletList { .. } => format!("<ul>{}</ul>", inline()),
            Self::OrderedList { attrs, .. } => {
                format!("<ol start=\"{}\">{}</ol>", attrs.start, inline())
            }
            Self::ListItem { .. } => format!("<li>{}</li>", inline()),
            Self::CodeBlock { attrs, content } => {
                let code = content
                    .iter()
                    .filter_map(|c| {
                        if let Self::Text { text, .. } = c {
                            Some(escape_html(text))
                        } else {
                            None
                        }
                    })
                    .collect::<String>();
                let language = attrs
                    .as_ref()
                    .and_then(|a| a.language.as_deref())
                    .unwrap_or("");
                format!(
                    "<pre><code data-language=\"{}\">{code}</code></pre>",
                    escape_html(language)
                )
            }
            Self::Table { .. } => format!("<table><tbody>{}</tbody></table>", inline()),
            Self::TableRow { .. } => format!("<tr>{}</tr>", inline()),
            Self::TableHeader { attrs, .. } | Self::TableCell { attrs, .. } => {
                let tag = if matches!(self, Self::TableHeader { .. }) {
                    "th"
                } else {
                    "td"
                };
                let mut properties = String::new();
                if let Some(attrs) = attrs {
                    if attrs.colspan > 1 {
                        properties.push_str(&format!(" colspan=\"{}\"", attrs.colspan));
                    }
                    if attrs.rowspan > 1 {
                        properties.push_str(&format!(" rowspan=\"{}\"", attrs.rowspan));
                    }
                    if let Some(alignment) = attrs.alignment {
                        properties.push_str(&format!(
                            " style=\"text-align:{}\"",
                            match alignment {
                                CellAlignment::Left => "left",
                                CellAlignment::Center => "center",
                                CellAlignment::Right => "right",
                            }
                        ));
                    }
                    if let Some(widths) = &attrs.colwidth {
                        properties.push_str(&format!(
                            " data-colwidth=\"{}\"",
                            widths
                                .iter()
                                .map(u16::to_string)
                                .collect::<Vec<_>>()
                                .join(",")
                        ));
                    }
                }
                format!("<{tag}{properties}>{}</{tag}>", inline())
            }
            Self::Media { attrs } => format!(
                "<figure><img src=\"{}\" alt=\"{}\"><figcaption>{}</figcaption></figure>",
                attrs.relative_path(),
                escape_html(&attrs.alt),
                escape_html(&attrs.caption)
            ),
        }
    }
}
fn validate_table(rows: &[RichNode]) -> Result<(), AppError> {
    if rows.len() > MAX_TABLE_ROWS {
        return Err(invalid("table row limit exceeded"));
    }
    let mut occupancy = vec![0usize; MAX_TABLE_COLUMNS];
    let mut width = None;
    for (row_index, row) in rows.iter().enumerate() {
        let mut column = 0;
        for cell in row.children() {
            while column < MAX_TABLE_COLUMNS && occupancy[column] > row_index {
                column += 1;
            }
            let attrs = match cell {
                RichNode::TableHeader { attrs, .. } | RichNode::TableCell { attrs, .. } => {
                    attrs.as_ref()
                }
                _ => None,
            };
            let colspan = attrs.map_or(1, |a| usize::from(a.colspan));
            let rowspan = attrs.map_or(1, |a| usize::from(a.rowspan));
            if column + colspan > MAX_TABLE_COLUMNS || row_index + rowspan > rows.len() {
                return Err(invalid("table cell extends past table boundary"));
            }
            for occupied in &mut occupancy[column..column + colspan] {
                if *occupied > row_index {
                    return Err(invalid("overlapping table cell span"));
                }
                *occupied = row_index + rowspan;
            }
            column += colspan;
        }
        let row_width = occupancy
            .iter()
            .rposition(|i| *i > row_index)
            .map_or(0, |i| i + 1);
        if row_width == 0 || width.is_some_and(|w| w != row_width) {
            return Err(invalid("table rows must have equal column widths"));
        }
        width = Some(row_width);
        if occupancy[..row_width].iter().any(|i| *i <= row_index) {
            return Err(invalid("table row has a missing cell"));
        }
    }
    Ok(())
}
fn inline_markdown(children: &[RichNode]) -> String {
    children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            let marked =
                |node: &RichNode| matches!(node, RichNode::Text { marks, .. } if !marks.is_empty());
            if marked(child)
                && (index > 0 && marked(&children[index - 1])
                    || children.get(index + 1).is_some_and(marked))
            {
                child.html()
            } else {
                child.markdown()
            }
        })
        .collect()
}
fn list_markdown(items: &[RichNode], start: Option<u32>) -> String {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let prefix = match start {
                Some(start) => format!("{}. ", u64::from(start) + index as u64),
                None => "- ".into(),
            };
            // Continuations belong under the list item's content column. A
            // two-space indent misparses 10.+ markers and nested paragraphs.
            let continuation = format!("\n{}", " ".repeat(prefix.len()));
            let body = item.markdown().replace('\n', &continuation);
            format!("{prefix}{body}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub fn escape_markdown(text: &str) -> String {
    let mut output = String::new();
    for ch in text.chars() {
        // Backslash escapes do not disable raw HTML or character references
        // in Markdown. Encode these before handling Markdown punctuation.
        if ch == '&' {
            output.push_str("&amp;");
            continue;
        }
        if ch == '<' {
            output.push_str("&lt;");
            continue;
        }
        if matches!(
            ch,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '>'
                | '|'
                | '~'
        ) {
            output.push('\\');
        }
        output.push(ch);
    }
    output
}
