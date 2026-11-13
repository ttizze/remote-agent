//! Rich Edit receives an RTF document built from Core's already parsed runs.
//! No Markdown or application commands are interpreted by the Windows control.
use serde_json::Value;
use std::fmt::Write;

#[derive(Clone, Default)]
pub(crate) struct Document {
    pub rtf: Vec<u8>,
    pub text: String,
    pub links: Vec<Link>,
}

#[derive(Clone)]
pub(crate) struct Link {
    pub start: usize,
    pub end: usize,
    pub destination: String,
}

struct Writer {
    body: String,
    text: String,
    links: Vec<Link>,
    colors: Vec<u32>,
}

pub(crate) fn paragraphs(blocks: &[Value]) -> Document {
    let mut writer = Writer::new();
    for (index, block) in blocks.iter().enumerate() {
        if index != 0 {
            writer.separator("\\par\n", '\r');
        }
        writer.paragraph(block, false);
    }
    writer.finish()
}

#[cfg(windows)]
pub(crate) fn picture(data: &str, width: u32) -> Option<Document> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let kind = if bytes.starts_with(b"\x89PNG") {
        "pngblip"
    } else if bytes.starts_with(b"\xff\xd8") {
        "jpegblip"
    } else {
        return None;
    };
    let mut writer = Writer::new();
    let _ = write!(writer.body, "{{\\pict\\{kind}\\picwgoal{} ", width * 20);
    for byte in bytes {
        let _ = write!(writer.body, "{byte:02x}");
    }
    writer.body.push('}');
    writer.text.push('\u{fffc}');
    Some(writer.finish())
}

pub(crate) fn table(rows: &[Value], width: u32) -> Document {
    let mut writer = Writer::new();
    for row in rows {
        let cells = row.as_array().map(Vec::as_slice).unwrap_or_default();
        writer.body.push_str("\\trowd\\trgaph120");
        for column in 1..=cells.len() {
            let edge =
                width.saturating_mul(20).saturating_mul(column as u32) / cells.len().max(1) as u32;
            let _ = write!(writer.body, "\\clbrdrb\\brdrs\\brdrw10\\cellx{edge}");
        }
        writer.body.push('\n');
        for cell in cells {
            let parts = cell.as_array().map(Vec::as_slice).unwrap_or_default();
            writer.body.push_str("\\pard\\intbl ");
            for part in parts {
                if let Some(blocks) = part["Text"].as_array() {
                    for block in blocks {
                        writer.paragraph(block, true);
                    }
                } else if let Some(code) = part.get("Code") {
                    if let Some(blocks) = code["blocks"].as_array() {
                        for block in blocks {
                            writer.paragraph(block, true);
                        }
                    }
                }
            }
            writer.separator("\\cell ", '\t');
        }
        writer.separator("\\row\n", '\r');
    }
    writer.finish()
}

impl Writer {
    fn new() -> Self {
        Self {
            body: String::new(),
            text: String::new(),
            links: Vec::new(),
            colors: vec![0xd4d4d4, 0x60a5fa, 0x313131, 0x6b5300, 0x9a6700],
        }
    }
    fn finish(self) -> Document {
        let mut rtf = String::from(
            "{\\rtf1\\ansi\\deff0\\uc1{\\fonttbl{\\f0 Segoe UI;}{\\f1 Consolas;}}{\\colortbl;",
        );
        for color in self.colors {
            let _ = write!(
                rtf,
                "\\red{}\\green{}\\blue{};",
                color >> 16 & 255,
                color >> 8 & 255,
                color & 255
            );
        }
        rtf.push_str("}\\viewkind4\\cf1\\f0\\fs28 ");
        rtf.push_str(&self.body);
        rtf.push('}');
        Document {
            rtf: rtf.into_bytes(),
            text: self.text,
            links: self.links,
        }
    }
    fn color(&mut self, rgb: u32) -> usize {
        if let Some(index) = self.colors.iter().position(|color| *color == rgb) {
            index + 1
        } else {
            self.colors.push(rgb);
            self.colors.len()
        }
    }
    fn separator(&mut self, rtf: &str, text: char) {
        self.body.push_str(rtf);
        self.text.push(text);
    }
    fn paragraph(&mut self, block: &Value, in_table: bool) {
        self.body
            .push_str(if in_table { "\\pard\\intbl" } else { "\\pard" });
        let indent = block["depth"]
            .as_u64()
            .unwrap_or_default()
            .saturating_sub(1)
            * 440
            + u64::from(block["quoted"] == true) * 240;
        let heading = block["header"].as_u64().and_then(|level| {
            [52, 46, 42, 38, 36, 36]
                .get(level.checked_sub(1)? as usize)
                .copied()
        });
        let size = heading.unwrap_or(if block["code"] == true { 26 } else { 28 });
        let font = u8::from(block["code"] == true);
        let alignment = match block["alignment"].as_str() {
            Some("center") => "\\qc",
            Some("right") => "\\qr",
            _ => "\\ql",
        };
        let _ = write!(
            self.body,
            "{alignment}\\li{indent}\\sa{}\\f{font}\\fs{size} ",
            if in_table { 0 } else { 180 }
        );
        if let Some(marker) = block["marker"].as_str() {
            self.literal(&format!("{marker} "));
        }
        for run in block["runs"].as_array().into_iter().flatten() {
            self.run(run, font, size, heading.is_some());
        }
    }
    fn run(&mut self, run: &Value, font: u8, size: u32, heading: bool) {
        let text = run["text"].as_str().unwrap_or_default();
        let link = run["link"].as_str();
        self.body.push('{');
        let font = if run["code"] == true { 1 } else { font };
        let color = run["darkColor"]
            .as_u64()
            .or_else(|| run["lightColor"].as_u64())
            .and_then(|rgb| u32::try_from(rgb).ok())
            .map(|rgb| self.color(rgb))
            .unwrap_or(if link.is_some() { 2 } else { 1 });
        let _ = write!(
            self.body,
            "\\f{font}\\fs{size}\\cf{color}\\b{}\\i{}\\strike{} ",
            u8::from(heading || run["strong"] == true),
            u8::from(run["emphasis"] == true),
            u8::from(run["strikethrough"] == true)
        );
        if let Some(destination) = link {
            self.body.push_str("{\\field{\\*\\fldinst HYPERLINK \"");
            escape(destination, &mut self.body);
            self.body.push_str("\"}{\\fldrslt\\ul ");
        }
        let start = self.text.encode_utf16().count();
        self.highlighted(text, run);
        if let Some(destination) = link {
            self.body.push_str("}}");
            self.links.push(Link {
                start,
                end: self.text.encode_utf16().count(),
                destination: destination.into(),
            });
        }
        self.body.push('}');
    }
    fn highlighted(&mut self, text: &str, run: &Value) {
        let mut offset = 0;
        for found in run["highlights"].as_array().into_iter().flatten() {
            let start = found["start"].as_u64().unwrap_or_default() as usize;
            let end = found["end"].as_u64().unwrap_or_default() as usize;
            let (Some(before), Some(selected)) = (text.get(offset..start), text.get(start..end))
            else {
                continue;
            };
            self.literal(before);
            self.body.push_str(if found["active"] == true {
                "{\\highlight5 "
            } else {
                "{\\highlight4 "
            });
            self.literal(selected);
            self.body.push('}');
            offset = end;
        }
        if let Some(rest) = text.get(offset..) {
            self.literal(rest);
        }
    }
    fn literal(&mut self, text: &str) {
        escape(text, &mut self.body);
        self.text.push_str(text);
    }
}

fn escape(text: &str, output: &mut String) {
    for unit in text.encode_utf16() {
        match unit {
            0x5c | 0x7b | 0x7d => {
                output.push('\\');
                output.push(char::from_u32(unit.into()).unwrap());
            }
            0x22 => output.push_str("\\'22"),
            0x0a => output.push_str("\\line "),
            0x0d => {}
            0x09 => output.push_str("\\tab "),
            0x20..=0x7e => output.push(char::from_u32(unit.into()).unwrap()),
            _ => {
                let _ = write!(output, "\\u{}?", unit as i16);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn selected_text_keeps_unicode_and_literal_code_while_rtf_escapes_commands() {
        let text = "日本語 👋 {\\rtf1} \"quoted\"\nsecond";
        let doc = paragraphs(&[json!({"code":true,"runs":[{"text":text}]})]);
        assert_eq!(doc.text, text);
        let rtf = String::from_utf8(doc.rtf).unwrap();
        assert!(rtf.contains("\\{\\\\rtf1\\}"));
        assert!(rtf.contains("\\u-10179?\\u-9141?"));
        assert!(rtf.contains("\\line second"));
    }
    #[test]
    fn links_keep_visible_labels_and_utf16_ranges_without_copying_destinations() {
        let doc = paragraphs(&[
            json!({"runs":[{"text":"👋 "},{"text":"説明","link":"https://example.test/a?q=1"}]}),
        ]);
        assert_eq!(doc.text, "👋 説明");
        assert_eq!((doc.links[0].start, doc.links[0].end), (3, 5));
        assert_eq!(doc.links[0].destination, "https://example.test/a?q=1");
    }
}
