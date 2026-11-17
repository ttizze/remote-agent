use super::*;
use std::mem::ManuallyDrop;
use windows::Win32::UI::Controls::EM_REPLACESEL;
use windows::core::{HRESULT, Result};

pub(super) fn insert(window: HWND, image: &rich_document::InlineImage) -> Result<()> {
    let mut range = CHARRANGE {
        cpMin: image.start as i32,
        cpMax: image.start as i32 + 1,
    };
    let stream = unsafe { SHCreateMemStream(Some(&image.bytes)) }.ok_or(E_OUTOFMEMORY)?;
    // Document dimensions are points, as in the RTF. The image API uses
    // hundredths of a millimetre (2540 per inch, 72 points per inch).
    let parameters = RICHEDIT_IMAGE_PARAMETERS {
        xWidth: (u64::from(image.width) * 2540 / 72).min(i32::MAX as u64) as i32,
        yHeight: (u64::from(image.height) * 2540 / 72).min(i32::MAX as u64) as i32,
        Type: TA_BASELINE.0 as i32,
        pwszAlternateText: w!(""),
        pIStream: ManuallyDrop::new(Some(stream)),
        ..Default::default()
    };
    let readonly = unsafe { GetWindowLongPtrW(window, GWL_STYLE) } & ES_READONLY as isize != 0;
    let length = message(window, WM_GETTEXTLENGTH, 0, 0).0;
    // The UI is read-only. Allow this synchronous insertion, then restore its
    // edit lock before reporting either success or a decoding failure.
    message(window, EM_SETREADONLY, 0, 0);
    message(
        window,
        EM_EXSETSEL,
        0,
        (&range as *const CHARRANGE) as isize,
    );
    let result = HRESULT(
        message(
            window,
            EM_INSERTIMAGE,
            0,
            (&parameters as *const RICHEDIT_IMAGE_PARAMETERS) as isize,
        )
        .0 as i32,
    );
    if result.is_err() {
        // Decoding can fail after RichEdit removes the selected placeholder.
        // Restore that slot before later images and links use its UTF-16 position.
        let remaining = (1 + message(window, WM_GETTEXTLENGTH, 0, 0).0 - length).max(0);
        range.cpMax = range.cpMin + remaining as i32;
        message(
            window,
            EM_EXSETSEL,
            0,
            (&range as *const CHARRANGE) as isize,
        );
        message(window, EM_REPLACESEL, 0, w!(" ").as_ptr() as isize);
    }
    message(window, EM_SETREADONLY, usize::from(readonly), 0);
    // This packed Win32 structure does not release its owned stream.
    drop(ManuallyDrop::into_inner(parameters.pIStream));
    result.ok()
}
