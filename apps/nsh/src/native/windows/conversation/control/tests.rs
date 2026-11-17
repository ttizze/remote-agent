use super::*;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};

/// Exercise the actual system parser: object and table delimiters affect link,
/// search and selection positions even when they have no displayed glyph.
#[test]
fn system_rich_edit_keeps_document_positions_for_chips_unicode_and_tables() {
    // Match the native UI thread: WIC image decoding needs COM initialized.
    unsafe { OleInitialize(None) }.unwrap();
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            rich_edit_class(),
            w!(""),
            WS_POPUP | WINDOW_STYLE(0x4 | 0x800),
            0,
            0,
            640,
            480,
            None,
            None,
            None,
            None,
        )
    }
    .unwrap();
    let chip: Value = serde_json::from_str(include_str!(
        "../../../../../apple/Tests/BexNshNativeTests/Fixtures/file-chip.json"
    ))
    .unwrap();
    let text = json!({"runs":[{"text":"👋 日本語\nnext"}]});
    let table = json!([
        [[{"Text":[{"runs":[{"text":"header"}]}]}]],
        [[
            {"Text":[{"runs":[{"text":"説明","link":"https://example.test","webMenu":[]}]}]},
            {"Image":{"source":"chart.png"}},
            {"Text":[{"runs":[{"text":"after image","link":"https://example.test/after","webMenu":[]}]}]}
        ]]
    ]);
    let media = json!({"chart.png":chip["runs"][0]["file"]["icon"]});
    let mut damaged = rich_document::paragraphs(&[chip.clone(), chip.clone()]);
    damaged.images[0].bytes.truncate(24);
    for (document, expected) in [
        (
            rich_document::paragraphs(&[chip.clone(), text, chip.clone()]),
            None,
        ),
        (
            rich_document::table(
                table.as_array().unwrap(),
                400,
                &media,
                &Value::Null,
                &Value::Null,
            ),
            None,
        ),
        // One broken image leaves its reserved space. Later images and every
        // link still load, and their UTF-16 positions remain unchanged.
        (damaged, Some("  統合前レビュー\r\u{fffc} 統合前レビュー")),
    ] {
        load_document(window, &document).unwrap();
        assert_ne!(
            unsafe { GetWindowLongPtrW(window, GWL_STYLE) } & ES_READONLY as isize,
            0,
            "image updates, including decoding failures, preserve the edit lock"
        );
        let mut buffer = vec![0u16; 4096];
        let mut request = GETTEXTEX {
            cb: (buffer.len() * 2) as u32,
            flags: GT_RAWTEXT,
            codepage: 1200,
            ..Default::default()
        };
        let length = message(
            window,
            EM_GETTEXTEX,
            (&mut request as *mut GETTEXTEX) as usize,
            buffer.as_mut_ptr() as isize,
        )
        .0 as usize;
        let native = String::from_utf16(&buffer[..length]).unwrap();
        assert_eq!(
            native.replace('\u{b}', "\n"),
            expected.unwrap_or(&document.text)
        );
        for link in &document.links {
            let native_label = String::from_utf16(&buffer[link.start..link.end]).unwrap();
            assert!(native_label.contains(&document.selected(link.start, link.end)));
            let range = CHARRANGE {
                cpMin: link.start as i32,
                cpMax: link.end as i32,
            };
            message(
                window,
                EM_EXSETSEL,
                0,
                (&range as *const CHARRANGE) as isize,
            );
            let mut format = CHARFORMATW {
                cbSize: std::mem::size_of::<CHARFORMATW>() as u32,
                ..Default::default()
            };
            message(
                window,
                EM_GETCHARFORMAT,
                SCF_SELECTION as usize,
                (&mut format as *mut CHARFORMATW) as isize,
            );
            assert_ne!(format.dwEffects & CFE_LINK, CFE_EFFECTS(0));
        }
    }
    unsafe {
        DestroyWindow(window).unwrap();
        OleUninitialize();
    }
}
