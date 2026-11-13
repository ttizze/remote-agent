use super::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

/// Exercise the actual system parser: object and table delimiters affect link,
/// search and selection positions even when they have no displayed glyph.
#[test]
fn system_rich_edit_keeps_document_positions_for_chips_unicode_and_tables() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.unwrap();
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
        [[{"Text":[{"runs":[{"text":"説明","link":"https://example.test","webMenu":[]}]}]}]]
    ]);
    for document in [
        rich_document::paragraphs(&[chip, text]),
        rich_document::table(table.as_array().unwrap(), 400),
    ] {
        let mut input = std::io::Cursor::new(document.rtf.as_slice());
        let mut stream = EDITSTREAM {
            dwCookie: (&mut input as *mut std::io::Cursor<&[u8]>) as usize,
            pfnCallback: Some(read_stream),
            ..Default::default()
        };
        message(
            window,
            EM_STREAMIN,
            SF_RTF as usize,
            (&mut stream as *mut EDITSTREAM) as isize,
        );
        assert_eq!(stream.dwError, 0);
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
        assert_eq!(native.replace('\u{b}', "\n"), document.text);
        for link in &document.links {
            let native_label = String::from_utf16(&buffer[link.start..link.end]).unwrap();
            assert!(native_label.contains(&document.selected(link.start, link.end)));
        }
    }
    unsafe {
        DestroyWindow(window).unwrap();
        CoUninitialize();
    }
}
