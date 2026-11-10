//! UIDocumentPicker imports copies and exports local files using Files.
use nsh_core::JsonValue;
use nsh_platform_ios::{ComponentContext, NativeComponent};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController, UIView};
use objc2_uniform_type_identifiers::UTType;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ComponentContext]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UIDocumentPickerDelegate for Delegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn picked(&self, _: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            self.ivars().emit(
                "picked",
                urls.iter()
                    .filter_map(|url| url.path().map(|path| JsonValue::String(path.to_string())))
                    .collect::<Vec<_>>()
                    .into(),
            );
        }
        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _: &UIDocumentPickerViewController) {
            self.ivars().emit("picked", JsonValue::Array(Vec::new()));
        }
    }
);
impl Delegate {
    fn new(context: ComponentContext) -> Retained<Self> {
        let this = Self::alloc(MainThreadMarker::new().unwrap()).set_ivars(context);
        unsafe { msg_send![super(this), init] }
    }
}

pub(super) struct DocumentPicker {
    view: Retained<UIView>,
    delegate: Retained<Delegate>,
    picker: Option<Retained<UIDocumentPickerViewController>>,
}
impl DocumentPicker {
    pub fn new(context: ComponentContext) -> Self {
        let delegate = Delegate::new(context);
        let view = UIView::initWithFrame(UIView::alloc(delegate.mtm()), CGRect::ZERO);
        Self {
            view,
            delegate,
            picker: None,
        }
    }
}
impl NativeComponent for DocumentPicker {
    fn view(&self) -> &UIView {
        &self.view
    }
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        let export = (name == "export")
            .then(|| value.and_then(JsonValue::as_str))
            .flatten()
            .filter(|path| !path.is_empty());
        if export.is_none()
            && (name != "open" || value.and_then(JsonValue::as_u64).unwrap_or_default() == 0)
        {
            return;
        }
        if self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.presentingViewController().is_some())
        {
            return;
        }
        let Some(mut controller) = self
            .view
            .window()
            .and_then(|window| window.rootViewController())
        else {
            return;
        };
        while let Some(presented) = controller.presentedViewController() {
            controller = presented;
        }
        let picker = if let Some(path) = export {
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            UIDocumentPickerViewController::initForExportingURLs_asCopy(
                UIDocumentPickerViewController::alloc(self.view.mtm()),
                &NSArray::from_retained_slice(&[url]),
                true,
            )
        } else {
            let data = UTType::typeWithIdentifier(&NSString::from_str("public.data")).unwrap();
            let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
                UIDocumentPickerViewController::alloc(self.view.mtm()),
                &NSArray::from_retained_slice(&[data]),
                true,
            );
            picker.setAllowsMultipleSelection(true);
            picker
        };
        picker.setDelegate(Some(ProtocolObject::from_ref(&*self.delegate)));
        controller.presentViewController_animated_completion(&picker, true, None);
        self.picker = Some(picker);
    }
}

impl Drop for DocumentPicker {
    fn drop(&mut self) {
        if let Some(picker) = &self.picker
            && picker.presentingViewController().is_some()
        {
            picker.dismissViewControllerAnimated_completion(false, None);
        }
    }
}
