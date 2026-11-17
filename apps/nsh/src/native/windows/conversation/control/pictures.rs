use super::*;
use windows::{
    Win32::System::{
        Com::{
            IDataObject, STGM_CREATE, STGM_READWRITE, STGM_SHARE_EXCLUSIVE,
            StructuredStorage::{
                CreateILockBytesOnHGlobal, IStorage, StgCreateDocfileOnILockBytes,
            },
        },
        Ole::{
            DROPEFFECT, DROPEFFECT_NONE, IOleInPlaceFrame, IOleInPlaceUIWindow, IOleObject,
            OLEINPLACEFRAMEINFO,
        },
        SystemServices::{MODIFIERKEYS_FLAGS, RECO_FLAGS},
    },
    core::{BOOL, GUID, Interface, OutRef, Ref, Result, implement},
};

/// Rich Edit needs client-owned OLE storage when it reads inline RTF pictures.
/// Without it the system parser substitutes a space for each picture.
pub(super) fn install(window: HWND) {
    let callback: IRichEditOleCallback = PictureStorage.into();
    assert_ne!(
        message(window, EM_SETOLECALLBACK, 0, callback.as_raw() as isize).0,
        0
    );
    // EM_SETOLECALLBACK retains the callback until the control releases it.
}

#[implement(IRichEditOleCallback)]
struct PictureStorage;

impl IRichEditOleCallback_Impl for PictureStorage_Impl {
    fn GetNewStorage(&self) -> Result<IStorage> {
        unsafe {
            let bytes = CreateILockBytesOnHGlobal(None, true)?;
            StgCreateDocfileOnILockBytes(
                &bytes,
                STGM_CREATE | STGM_READWRITE | STGM_SHARE_EXCLUSIVE,
                0,
            )
        }
    }

    fn QueryInsertObject(&self, _: *mut GUID, _: Ref<IStorage>, _: i32) -> Result<()> {
        Ok(())
    }

    fn DeleteObject(&self, _: Ref<IOleObject>) -> Result<()> {
        Ok(())
    }

    fn GetInPlaceContext(
        &self,
        _: OutRef<IOleInPlaceFrame>,
        _: OutRef<IOleInPlaceUIWindow>,
        _: *mut OLEINPLACEFRAMEINFO,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn ShowContainerUI(&self, _: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryAcceptData(
        &self,
        _: Ref<IDataObject>,
        _: *mut u16,
        _: RECO_FLAGS,
        _: BOOL,
        _: HGLOBAL,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn ContextSensitiveHelp(&self, _: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetClipboardData(&self, _: *mut CHARRANGE, _: u32, _: OutRef<IDataObject>) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetDragDropEffect(
        &self,
        _: BOOL,
        _: MODIFIERKEYS_FLAGS,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        if !effect.is_null() {
            unsafe {
                *effect = DROPEFFECT_NONE;
            }
        }
        Ok(())
    }

    fn GetContextMenu(
        &self,
        _: RICH_EDIT_GET_CONTEXT_MENU_SEL_TYPE,
        _: Ref<IOleObject>,
        _: *mut CHARRANGE,
        _: *mut HMENU,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}
