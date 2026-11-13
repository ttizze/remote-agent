//! Rich Edit receives an RTF document built from Core's already parsed runs.
//! No Markdown or application commands are interpreted by the Windows control.
use serde_json::Value;
use std::fmt::Write;

#[derive(Clone, Default)]
pub(crate) struct Document {
    pub rtf: Vec<u8>,
    pub text: String,
    pub links: Vec<Link>,
    pub active: Option<(usize, usize)>,
    decorations: Vec<(usize, usize)>,
}

#[derive(Clone)]
pub(crate) struct Link {
    pub start: usize,
    pub end: usize,
    pub destination: String,
    pub menu: Value,
    pub action: &'static str,
    pub tooltip: String,
}

struct Writer {
    body: String,
    text: String,
    links: Vec<Link>,
    colors: Vec<u32>,
    units: usize,
    active: Option<(usize, usize)>,
    decorations: Vec<(usize, usize)>,
}

pub(crate) fn paragraphs(blocks: &[Value]) -> Document {
    let mut writer = Writer::new();
    for (index, block) in blocks.iter().enumerate() {
        if index != 0 {
            writer.separator("\\par\n", '\r');
        }
        writer.paragraph(block, None);
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
    let height = if kind == "pngblip" {
        let w = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        width.saturating_mul(h) / w.max(1)
    } else {
        width
    };
    let mut writer = Writer::new();
    let _ = write!(
        writer.body,
        "{{\\pict\\{kind}\\picwgoal{}\\pichgoal{} ",
        width * 20,
        height * 20
    );
    for byte in bytes {
        let _ = write!(writer.body, "{byte:02x}");
    }
    writer.body.push('}');
    writer.text.push('\u{fffc}');
    Some(writer.finish())
}

pub(crate) fn table(rows: &[Value], width: u32) -> Document {
    let mut writer = Writer::new();
    let columns = rows
        .iter()
        .filter_map(Value::as_array)
        .map(Vec::len)
        .max()
        .unwrap_or_default() as u32;
    let width = width.max(columns.saturating_mul(160));
    for (index, row) in rows.iter().enumerate() {
        // Rich Edit's in-memory table rows have hidden delimiter paragraphs.
        // Count them so native selection and link positions address the cells.
        writer.structure("\u{fff9}\r");
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
                        writer.paragraph(block, Some(index == 0));
                    }
                } else if let Some(code) = part.get("Code")
                    && let Some(blocks) = code["blocks"].as_array()
                {
                    for block in blocks {
                        writer.paragraph(block, Some(index == 0));
                    }
                }
            }
            writer.separator("\\cell ", '\u{7}');
        }
        writer.structure("\u{fffb}");
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
            units: 0,
            active: None,
            decorations: Vec::new(),
            colors: vec![0xd4d4d4, 0x60a5fa, 0x313131, 0x6b5300, 0x9a6700],
        }
    }
    fn finish(self) -> Document {
        let mut rtf = String::from(
            "{\\rtf1\\ansi\\deff0\\uc1{\\fonttbl{\\f0 Segoe UI;}{\\f1 Consolas;}{\\f2 Segoe Fluent Icons;}{\\f3 Segoe UI Semibold;}}{\\colortbl;",
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
            active: self.active,
            decorations: self.decorations,
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
        self.units += text.len_utf16();
    }
    fn structure(&mut self, text: &str) {
        let start = self.units;
        self.text.push_str(text);
        self.units += text.encode_utf16().count();
        self.decorations.push((start, self.units));
    }
    fn paragraph(&mut self, block: &Value, table_header: Option<bool>) {
        let in_table = table_header.is_some();
        self.body
            .push_str(if in_table { "\\pard\\intbl" } else { "\\pard" });
        let indent = block["depth"]
            .as_u64()
            .unwrap_or_default()
            .saturating_sub(1)
            * 440
            + u64::from(block["quoted"] == true) * 240;
        let heading = block["header"].as_u64().and_then(|level| {
            [40, 36, 32, 28, 28, 28]
                .get(level.checked_sub(1)? as usize)
                .copied()
        });
        let size = block["size"]
            .as_u64()
            .map(|size| (size * 2) as u32)
            .unwrap_or_else(|| {
                heading.unwrap_or(if in_table {
                    24
                } else if block["code"] == true {
                    26
                } else {
                    28
                })
            });
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
        let runs = block["runs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (index, run) in runs.iter().enumerate() {
            self.run(
                run,
                font,
                size,
                heading.is_some() || table_header == Some(true),
                caption(runs, index),
                index > 0 && caption(runs, index - 1),
            );
        }
    }
    fn run(
        &mut self,
        run: &Value,
        font: u8,
        size: u32,
        heading: bool,
        caption: bool,
        after_caption: bool,
    ) {
        let text = run["text"].as_str().unwrap_or_default();
        let link = run["link"].as_str();
        let file = &run["file"];
        self.body.push('{');
        let font = if file.is_object() {
            3
        } else if run["code"] == true {
            1
        } else {
            font
        };
        let color = file["darkColor"]
            .as_u64()
            .or_else(|| {
                run["darkColor"]
                    .as_u64()
                    .or_else(|| run["lightColor"].as_u64())
            })
            .and_then(|rgb| u32::try_from(rgb).ok())
            .map(|rgb| self.color(rgb))
            .unwrap_or(if link.is_some() && !caption { 2 } else { 1 });
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
            self.body.push_str(if file.is_object() || caption {
                "\"}{\\fldrslt\\ul0 "
            } else {
                "\"}{\\fldrslt\\ul "
            });
        }
        let start = self.units;
        if file.is_object() && after_caption {
            self.literal(" ");
        }
        self.file_glyph(file, size);
        self.highlighted(text, run);
        if let Some(destination) = link {
            self.body.push_str("}}");
            self.links.push(Link {
                start,
                end: self.units,
                destination: destination.into(),
                menu: run
                    .get("file")
                    .map(|file| file["menu"].clone())
                    .unwrap_or_else(|| run["webMenu"].clone()),
                action: if run["file"].is_object() {
                    "file"
                } else {
                    "webLink"
                },
                tooltip: file["tooltip"].as_str().unwrap_or(destination).to_owned(),
            });
        }
        self.body.push('}');
    }
    fn file_glyph(&mut self, file: &Value, size: u32) {
        use base64::Engine;
        let start = self.units;
        if file["folder"] == true {
            self.body.push_str("{\\f2 ");
            self.literal("\u{e8b7}");
            self.body.push('}');
        } else if let Some(bytes) = file["icon"]
            .as_str()
            .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .filter(|bytes| bytes.starts_with(b"\x89PNG"))
        {
            let _ = write!(
                self.body,
                "{{\\pict\\pngblip\\picwgoal{}\\pichgoal{} ",
                size * 10,
                size * 10
            );
            for byte in bytes {
                let _ = write!(self.body, "{byte:02x}");
            }
            self.body.push('}');
            self.text.push('\u{fffc}');
            self.units += 1;
        } else {
            return;
        }
        self.literal(" ");
        self.decorations.push((start, self.units));
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
            let begin = self.units;
            self.body.push_str(if found["active"] == true {
                "{\\highlight5 "
            } else {
                "{\\highlight4 "
            });
            self.literal(selected);
            if found["active"] == true {
                self.active = Some((self.active.map_or(begin, |range| range.0), self.units));
            }
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
        self.units += text.encode_utf16().count();
    }
}

impl Document {
    /// Native object positions remain in the control, while copying and quoting
    /// return labels without the decorative icons or their gap.
    pub fn selected(&self, start: usize, end: usize) -> String {
        let units: Vec<_> = self
            .text
            .encode_utf16()
            .enumerate()
            .filter(|(index, _)| {
                *index >= start
                    && *index < end
                    && !self
                        .decorations
                        .iter()
                        .any(|(from, to)| *from <= *index && *index < *to)
            })
            .map(|(_, unit)| unit)
            .collect();
        String::from_utf16_lossy(&units)
            .replace(['\r', '\u{b}'], "\n")
            .replace('\u{7}', "\t")
    }
}

fn caption(runs: &[Value], at: usize) -> bool {
    let run = &runs[at];
    if run["file"].is_object() || !run["link"].is_string() {
        return false;
    }
    runs[at + 1..]
        .iter()
        .find(|next| next["file"].is_object() || next["link"] != run["link"])
        .is_some_and(|next| next["file"].is_object() && next["link"] == run["link"])
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
    fn file_chips_keep_icons_accents_tooltips_menus_and_clean_copied_labels() {
        let block: Value = serde_json::from_str(include_str!(
            "../../apple/Tests/BexNshNativeTests/Fixtures/file-chip.json"
        ))
        .unwrap();
        let doc = paragraphs(&[block]);
        assert_eq!(doc.text, "\u{fffc} 統合前レビュー");
        assert_eq!(
            doc.selected(0, doc.text.encode_utf16().count()),
            "統合前レビュー"
        );
        assert_eq!(doc.links[0].tooltip, "/repo/SKILL.md");
        assert_eq!(doc.links[0].action, "file");
        assert_eq!(doc.links[0].menu[1]["id"], "\"CopyFullPath\"");
        let rtf = String::from_utf8(doc.rtf).unwrap();
        assert!(rtf.contains("\\pict\\pngblip"));
        assert!(rtf.contains("\\red255\\green0\\blue0;") && rtf.contains("\\f3\\fs28"));
        assert!(rtf.contains("\\fldrslt\\ul0"));
        let doc = paragraphs(&[
            json!({"runs":[{"text":"Read","link":"/repo"},{"text":"repo","link":"/repo","file":{"folder":true,"tooltip":"/repo"}}]}),
        ]);
        assert_eq!(
            doc.selected(0, doc.text.encode_utf16().count()),
            "Read repo"
        );
        assert_eq!(doc.links[1].start, 4);
    }
    #[test]
    fn native_tables_keep_cell_text_and_boundaries_without_markdown_separators() {
        let rows = serde_json::json!([[[{"Text":[{"runs":[{"text":"日本語"}]}]}],[{"Text":[{"runs":[{"text":"value"}]}]}]]]);
        let doc = table(rows.as_array().unwrap(), 400);
        assert_eq!(
            doc.selected(0, doc.text.encode_utf16().count()),
            "日本語\tvalue\t\n"
        );
        let rtf = String::from_utf8(doc.rtf).unwrap();
        assert!(rtf.contains("\\trowd"));
        assert!(rtf.contains("\\cellx8000"));
    }

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
            json!({"runs":[{"text":"👋 "},{"text":"説明","link":"https://example.test/a?q=1","webMenu":[{"id":"copyLink","title":"Copy link"}]}]}),
        ]);
        assert_eq!(doc.text, "👋 説明");
        assert_eq!((doc.links[0].start, doc.links[0].end), (3, 5));
        assert_eq!(doc.links[0].destination, "https://example.test/a?q=1");
        assert_eq!(doc.links[0].menu[0]["id"], "copyLink");
        assert_eq!(doc.links[0].action, "webLink");
    }
    #[test]
    fn active_search_ranges_use_utf16_across_unicode_and_formatted_runs() {
        let doc = paragraphs(&[
            json!({"runs":[{"text":"👋 before "},{"text":"é","highlights":[{"start":0,"end":2,"active":true}]},{"text":"日本","strong":true,"highlights":[{"start":0,"end":6,"active":true}]}]}),
        ]);
        assert_eq!(doc.active, Some((10, 13)));
        let doc = paragraphs(&[
            json!({"runs":[{"text":"other","highlights":[{"start":0,"end":5,"active":false}]}]}),
        ]);
        assert_eq!(doc.active, None);
    }
}
