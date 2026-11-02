use std::ffi::{CStr, c_char, c_void};
use std::ptr;

type CfTypeRef = *const c_void;
type CfStringRef = *const c_void;
type AxUiElementRef = *const c_void;

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const MESSAGING_TIMEOUT_SECS: f32 = 0.25;
/// Each call while finding Hex buddy's target. The caller also supplies an
/// overall deadline and admission check, tested before each query.
const CARET_TIMEOUT_SECS: f32 = 0.05;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AxUiElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AxUiElementRef,
        attribute: CfStringRef,
        value: *mut CfTypeRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: AxUiElementRef, timeout_in_seconds: f32) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AxUiElementRef,
        attribute: CfStringRef,
        value: CfTypeRef,
    ) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: AxUiElementRef,
        parameterized_attribute: CfStringRef,
        parameter: CfTypeRef,
        result: *mut CfTypeRef,
    ) -> i32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueCreate(value_type: u32, value: *const c_void) -> CfTypeRef;
    fn AXValueGetValue(value: CfTypeRef, value_type: u32, value_out: *mut c_void) -> bool;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

/// `kAXValueTypeCGPoint`, `kAXValueTypeCGSize`, `kAXValueTypeCGRect`.
const AX_VALUE_CG_POINT: u32 = 1;
const AX_VALUE_CG_SIZE: u32 = 2;
const AX_VALUE_CG_RECT: u32 = 3;

/// `kAXValueTypeCFRange`.
const AX_VALUE_CF_RANGE: u32 = 4;

/// Core Foundation's `CFRange`, in UTF-16 code units.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CfRange {
    location: isize,
    length: isize,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(value: CfTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(value: CfStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        value: CfStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        string: *const c_char,
        encoding: u32,
    ) -> CfStringRef;
    fn CFURLGetTypeID() -> usize;
    fn CFURLGetString(url: CfTypeRef) -> CfStringRef;
    fn CFRelease(value: CfTypeRef);
    fn CFRetain(value: CfTypeRef) -> CfTypeRef;
    fn CFEqual(left: CfTypeRef, right: CfTypeRef) -> u8;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CfTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfTypeRef, index: isize) -> CfTypeRef;
    static kCFBooleanTrue: CfTypeRef;
}

/// A rectangle in Quartz global points: from the primary display's
/// top-left corner, y down (Rune's global space).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ScreenRect {
    pub fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    fn finite(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
    }

    /// Whether `inner` lies inside, give or take `slack` points.
    fn holds(self, inner: Self, slack: f64) -> bool {
        inner.x >= self.x - slack
            && inner.y >= self.y - slack
            && inner.x + inner.width <= self.x + self.width + slack
            && inner.y + inner.height <= self.y + self.height + slack
    }
}

/// Where a paste into the frontmost app went, for Labs › Hex buddy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaretTarget {
    /// The insertion point itself.
    Caret(ScreenRect),
    /// Only the focused text field's frame.
    Field(ScreenRect),
}

/// Whether the frontmost app's text is off limits: secure input is on
/// (a password field has focus somewhere).
pub fn secure_input() -> bool {
    // SAFETY: a plain Carbon query with no arguments.
    unsafe { IsSecureEventInputEnabled() }
}

/// A focused control retained on the animation worker. Reading a later
/// result through this snapshot cannot silently switch to another field.
pub(crate) struct CaretFocus {
    pid: i32,
    application: CfObject,
    focused: CfObject,
    window: CfObject,
}

/// Retains the current field and window, unless it is a password field.
/// `allowed` carries the caller's deadline, Labs, focus, and secure-input
/// checks; a query in progress can exceed its deadline by at most one AX
/// messaging timeout.
pub(crate) fn caret_focus(pid: i32, allowed: &impl Fn() -> bool) -> Option<CaretFocus> {
    let read = CaretRead { allowed };
    let application = read.application(pid)?;
    let focused = read.element(&application, c"AXFocusedUIElement")?;
    if read.string(&focused, c"AXSubrole").as_deref() == Some("AXSecureTextField") {
        return None;
    }
    let window = read.element(&application, c"AXFocusedWindow")?;
    allowed().then_some(CaretFocus {
        pid,
        application,
        focused,
        window,
    })
}

impl CaretFocus {
    /// Whether this exact control still has focus in the same app.
    pub(crate) fn is_current(&self, pid: i32, allowed: &impl Fn() -> bool) -> bool {
        if pid != self.pid {
            return false;
        }
        let read = CaretRead { allowed };
        let Some(current) = read.attribute(&self.application, c"AXFocusedUIElement") else {
            return false;
        };
        // SAFETY: Both values are retained AXUIElements.
        let same = unsafe { CFEqual(current.0, self.focused.0) != 0 };
        same && read.string(&self.focused, c"AXSubrole").as_deref() != Some("AXSecureTextField")
            && allowed()
    }

    /// The insertion point or field frame, excluding collapsed, off-window,
    /// and secure fields. Each read uses the same admission check.
    pub(crate) fn target(&self, allowed: &impl Fn() -> bool) -> Option<CaretTarget> {
        let read = CaretRead { allowed };
        if read.string(&self.focused, c"AXSubrole").as_deref() == Some("AXSecureTextField") {
            return None;
        }
        let window = read.frame(&self.window)?;
        let by_range = read
            .range(&self.focused, c"AXSelectedTextRange")
            .and_then(|selection| {
                if selection.location < 0 || selection.length < 0 {
                    return None;
                }
                read.bounds(
                    &self.focused,
                    CfRange {
                        location: selection.location.checked_add(selection.length)?,
                        length: 0,
                    },
                )
            });
        let rect = by_range
            .filter(|rect| plausible_caret(*rect, window))
            .or_else(|| {
                // Chromium and Electron may answer only with text markers.
                let markers = read.attribute(&self.focused, c"AXSelectedTextMarkerRange")?;
                read.parameterized_rect(&self.focused, c"AXBoundsForTextMarkerRange", &markers)
                    .filter(|rect| plausible_caret(*rect, window))
            });
        if let Some(rect) = rect {
            return allowed().then_some(CaretTarget::Caret(rect));
        }
        let role = read.string(&self.focused, c"AXRole");
        if !matches!(
            role.as_deref(),
            Some("AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField")
        ) {
            return None;
        }
        read.frame(&self.focused)
            .filter(|frame| plausible_field(*frame, window) && allowed())
            .map(CaretTarget::Field)
    }

    /// Finds the pasted words in this retained field. A lost admission
    /// drops the complete flight, including bounds read before it changed.
    pub(crate) fn words(
        &self,
        text: &str,
        limit: usize,
        allowed: &impl Fn() -> bool,
    ) -> Option<Vec<Option<ScreenRect>>> {
        let body = text.trim_start();
        let units = isize::try_from(body.encode_utf16().count()).ok()?;
        if units == 0 {
            return None;
        }
        let read = CaretRead { allowed };
        if read.string(&self.focused, c"AXSubrole").as_deref() == Some("AXSecureTextField") {
            return None;
        }
        let window = read.frame(&self.window)?;
        let selection = read.range(&self.focused, c"AXSelectedTextRange")?;
        let start = selection.location.checked_sub(units)?;
        if selection.length != 0 || start < 0 {
            return None;
        }
        let found = read.string_for_range(
            &self.focused,
            CfRange {
                location: start,
                length: units,
            },
        )?;
        if !found.split_whitespace().eq(body.split_whitespace()) {
            return None;
        }
        let mut rects = Vec::new();
        for (offset, units) in utf16_words(&found).take(limit) {
            let rect = read.bounds(
                &self.focused,
                CfRange {
                    location: start.checked_add(offset)?,
                    length: units,
                },
            );
            if !allowed() {
                return None;
            }
            rects.push(rect.filter(|rect| plausible_word(*rect, window)));
        }
        allowed().then_some(rects)
    }
}

/// Asks Chromium and Electron apps to build their accessibility tree, which
/// they keep asleep until an assistive app asks, so it's ready by the time a
/// dictation pastes. Other apps ignore it.
pub fn wake_text_markers(pid: i32, allowed: &impl Fn() -> bool) {
    let read = CaretRead { allowed };
    let Some(application) = read.application(pid) else {
        return;
    };
    let Some(attribute) = cf_string_literal(c"AXManualAccessibility") else {
        return;
    };
    let attribute = CfObject(attribute);
    if allowed() {
        // SAFETY: Both are valid Core Foundation objects; kCFBooleanTrue is a constant.
        unsafe { AXUIElementSetAttributeValue(application.0, attribute.0, kCFBooleanTrue) };
    }
}

/// Each whitespace-separated word's offset and length in UTF-16 units.
fn utf16_words(text: &str) -> impl Iterator<Item = (isize, isize)> + '_ {
    let mut offset = 0_isize;
    let mut start = None;
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        loop {
            let Some(ch) = chars.next() else {
                return start.take().map(|from| (from, offset - from));
            };
            let at = offset;
            offset += ch.len_utf16() as isize;
            match (ch.is_whitespace(), start) {
                (false, None) => start = Some(at),
                (true, Some(from)) => {
                    start = None;
                    return Some((from, at - from));
                }
                _ => {}
            }
        }
    })
}

/// A collapsed caret: line-height tall, a few points wide at most, inside
/// the focused window. Chromium reports an unanswered query as
/// `(0, primary height, 0, 0)`, which this refuses.
fn plausible_caret(rect: ScreenRect, window: ScreenRect) -> bool {
    rect.finite()
        && (4.0..=200.0).contains(&rect.height)
        && (0.0..=40.0).contains(&rect.width)
        && window.holds(rect, 8.0)
}

/// One pasted word's rect: on screen, inside the window, no taller than a
/// few lines (a word wrapped across lines reports their union).
fn plausible_word(rect: ScreenRect, window: ScreenRect) -> bool {
    rect.finite()
        && (4.0..=200.0).contains(&rect.height)
        && rect.width > 0.0
        && window.holds(rect, 8.0)
}

fn plausible_field(rect: ScreenRect, window: ScreenRect) -> bool {
    rect.finite() && rect.width >= 8.0 && rect.height >= 8.0 && window.holds(rect, 8.0)
}

pub fn focused_window_title(pid: i32) -> Option<String> {
    // SAFETY: AXUIElementCreateApplication returns a retained Core Foundation object.
    let application = bounded_element(
        unsafe { AXUIElementCreateApplication(pid) },
        MESSAGING_TIMEOUT_SECS,
    )?;
    let window = copy_element(&application, c"AXFocusedWindow", MESSAGING_TIMEOUT_SECS)?;
    copy_string(&window, c"AXTitle").filter(|title| !title.is_empty())
}

/// The page address of the web area holding the application's focused
/// element, for browsers without a scripting dictionary (Firefox, Zen).
/// The outermost web area wins, so focus inside an iframe still reports
/// the page itself.
pub fn focused_web_url(pid: i32) -> Option<String> {
    const MAX_DEPTH: usize = 64;
    // SAFETY: AXUIElementCreateApplication returns a retained Core Foundation object.
    let application = bounded_element(
        unsafe { AXUIElementCreateApplication(pid) },
        MESSAGING_TIMEOUT_SECS,
    )?;
    let mut element = copy_element(&application, c"AXFocusedUIElement", MESSAGING_TIMEOUT_SECS)?;
    let mut url = None;
    for _ in 0..MAX_DEPTH {
        if copy_string(&element, c"AXRole").as_deref() == Some("AXWebArea") {
            url = copy_url(&element, c"AXURL").or(url);
        }
        match copy_element(&element, c"AXParent", MESSAGING_TIMEOUT_SECS) {
            Some(parent) => element = parent,
            None => break,
        }
    }
    url
}

/// Every page a browser's windows show, for a call's address or tab title
/// when its scripting dictionary can't be asked (#831). Chromium (Arc,
/// Chrome, Brave, Edge) builds its web accessibility tree only for an
/// assistive app that asks, which `AXManualAccessibility` does, once per
/// process; without it a browser behind another app shows windows but no
/// web areas.
pub fn browser_pages(pid: i32) -> crate::dictation::browser_page::BrowserPages {
    // SAFETY: AXUIElementCreateApplication returns a retained Core Foundation object.
    let Some(application) = bounded_element(
        unsafe { AXUIElementCreateApplication(pid) },
        MESSAGING_TIMEOUT_SECS,
    ) else {
        return Default::default();
    };
    request_web_accessibility(pid, &application);
    let mut windows: Vec<CfObject> = Vec::new();
    let candidates = [
        copy_element(&application, c"AXFocusedWindow", MESSAGING_TIMEOUT_SECS),
        copy_element(&application, c"AXMainWindow", MESSAGING_TIMEOUT_SECS),
    ]
    .into_iter()
    .flatten()
    .chain(copy_elements(&application, c"AXWindows"));
    for window in candidates {
        // SAFETY: Both are live AXUIElements.
        if !windows
            .iter()
            .any(|known| unsafe { CFEqual(known.0, window.0) } != 0)
        {
            windows.push(window);
        }
    }
    let mut pages = crate::dictation::browser_page::pages(windows.into_iter().map(Element));
    // The focused web area, when there is one, is the likeliest page.
    if let Some(url) = focused_web_url(pid) {
        pages.urls.retain(|known| known != &url);
        pages.urls.insert(0, url);
    }
    pages
}

/// Asks a Chromium browser for its web accessibility tree, once per
/// process. Other apps ignore the attribute.
fn request_web_accessibility(pid: i32, application: &CfObject) {
    static ASKED: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());
    let mut asked = ASKED.lock().unwrap_or_else(|error| error.into_inner());
    if asked.contains(&pid) {
        return;
    }
    asked.push(pid);
    let Some(attribute) = cf_string_literal(c"AXManualAccessibility").map(CfObject) else {
        return;
    };
    // SAFETY: A live application element, a CFString name, and the shared
    // kCFBooleanTrue constant.
    let status =
        unsafe { AXUIElementSetAttributeValue(application.0, attribute.0, kCFBooleanTrue) };
    if status != 0 {
        tracing::debug!(
            status,
            pid,
            "the browser has no manual accessibility switch"
        );
    }
}

/// An element as `browser_page` walks it.
struct Element(CfObject);

impl crate::dictation::browser_page::AxNode for Element {
    fn role(&self) -> Option<String> {
        copy_string(&self.0, c"AXRole")
    }

    fn title(&self) -> Option<String> {
        copy_string(&self.0, c"AXTitle")
    }

    fn url(&self) -> Option<String> {
        copy_url(&self.0, c"AXURL")
    }

    fn children(&self) -> Vec<Self> {
        copy_elements(&self.0, c"AXChildren")
            .into_iter()
            .map(Element)
            .collect()
    }
}

/// An attribute holding elements (`AXWindows`, `AXChildren`), each bounded.
fn copy_elements(element: &CfObject, name: &CStr) -> Vec<CfObject> {
    let Some(array) = copy_attribute(element, name) else {
        return Vec::new();
    };
    if unsafe { CFGetTypeID(array.0) } != unsafe { CFArrayGetTypeID() } {
        return Vec::new();
    }
    let count = unsafe { CFArrayGetCount(array.0) };
    (0..count)
        .filter_map(|index| {
            // SAFETY: Get rule: the array owns the item, so it is retained
            // before the array goes.
            let item = unsafe { CFArrayGetValueAtIndex(array.0, index) };
            if item.is_null() {
                return None;
            }
            let item = CfObject(unsafe { CFRetain(item) });
            (unsafe { AXUIElementSetMessagingTimeout(item.0, MESSAGING_TIMEOUT_SECS) } == 0)
                .then_some(item)
        })
        .collect()
}

/// The character just before the insertion point of the application's
/// focused text element, for Labs › Smart spacing. `None` when text is
/// selected, the cursor is at the start, or the app doesn't say.
pub fn char_before_caret(pid: i32) -> Option<char> {
    // SAFETY: AXUIElementCreateApplication returns a retained Core Foundation object.
    let application = bounded_element(
        unsafe { AXUIElementCreateApplication(pid) },
        MESSAGING_TIMEOUT_SECS,
    )?;
    let focused = copy_element(&application, c"AXFocusedUIElement", MESSAGING_TIMEOUT_SECS)?;
    let selection = copy_range(&focused, c"AXSelectedTextRange")?;
    if selection.length != 0 || selection.location <= 0 {
        return None;
    }
    // One UTF-16 unit, or two when it is half of a surrogate pair.
    (1..=2.min(selection.location)).find_map(|units| {
        let range = CfRange {
            location: selection.location - units,
            length: units,
        };
        copy_string_for_range(&focused, range)?.chars().next_back()
    })
}

fn copy_range(element: &CfObject, name: &CStr) -> Option<CfRange> {
    let value = copy_attribute(element, name)?;
    if unsafe { CFGetTypeID(value.0) } != unsafe { AXValueGetTypeID() } {
        return None;
    }
    let mut range = CfRange::default();
    // SAFETY: `value` is an AXValue and `range` has CFRange's layout.
    unsafe {
        AXValueGetValue(
            value.0,
            AX_VALUE_CF_RANGE,
            (&mut range as *mut CfRange).cast(),
        )
    }
    .then_some(range)
}

fn copy_string_for_range(element: &CfObject, range: CfRange) -> Option<String> {
    // SAFETY: AXValueCreate copies the CFRange and returns a retained object.
    let parameter = unsafe { AXValueCreate(AX_VALUE_CF_RANGE, (&range as *const CfRange).cast()) };
    if parameter.is_null() {
        return None;
    }
    let parameter = CfObject(parameter);
    let attribute = CfObject(cf_string_literal(c"AXStringForRange")?);
    let mut value = ptr::null();
    let status = unsafe {
        AXUIElementCopyParameterizedAttributeValue(element.0, attribute.0, parameter.0, &mut value)
    };
    if status != 0 || value.is_null() {
        return None;
    }
    let value = CfObject(value);
    cf_string(value.0).filter(|text| !text.is_empty())
}

struct CfObject(CfTypeRef);

impl Drop for CfObject {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn bounded_element(element: AxUiElementRef, timeout: f32) -> Option<CfObject> {
    if element.is_null() {
        return None;
    }
    let element = CfObject(element);
    (unsafe { AXUIElementSetMessagingTimeout(element.0, timeout) } == 0).then_some(element)
}

fn copy_attribute(element: &CfObject, name: &CStr) -> Option<CfObject> {
    let attribute = CfObject(cf_string_literal(name)?);
    let mut value = ptr::null();
    let status = unsafe { AXUIElementCopyAttributeValue(element.0, attribute.0, &mut value) };
    // `then`, not `then_some`: dropping a `CfObject(null)` would CFRelease(NULL).
    (status == 0 && !value.is_null()).then(|| CfObject(value))
}

fn copy_element(element: &CfObject, name: &CStr, timeout: f32) -> Option<CfObject> {
    let child = copy_attribute(element, name)?;
    (unsafe { AXUIElementSetMessagingTimeout(child.0, timeout) } == 0).then_some(child)
}

/// Every AX call for a flight goes through its current admission check.
struct CaretRead<'a> {
    allowed: &'a dyn Fn() -> bool,
}

impl CaretRead<'_> {
    fn call<T>(&self, read: impl FnOnce() -> T) -> Option<T> {
        (self.allowed)().then(read)
    }

    fn application(&self, pid: i32) -> Option<CfObject> {
        // SAFETY: Create rule returns a retained element.
        let value = self.call(|| unsafe { AXUIElementCreateApplication(pid) })?;
        if value.is_null() {
            return None;
        }
        self.bounded(CfObject(value))
    }

    fn bounded(&self, element: CfObject) -> Option<CfObject> {
        // SAFETY: `element` is retained for the call.
        (self.call(|| unsafe { AXUIElementSetMessagingTimeout(element.0, CARET_TIMEOUT_SECS) })?
            == 0)
            .then_some(element)
    }

    fn attribute(&self, element: &CfObject, name: &CStr) -> Option<CfObject> {
        self.call(|| copy_attribute(element, name))?
    }

    fn element(&self, element: &CfObject, name: &CStr) -> Option<CfObject> {
        self.bounded(self.attribute(element, name)?)
    }

    fn string(&self, element: &CfObject, name: &CStr) -> Option<String> {
        cf_string(self.attribute(element, name)?.0)
    }

    fn value(&self, value: &CfObject, kind: u32, out: *mut c_void) -> Option<()> {
        // SAFETY: The type ID query has no side effects.
        if unsafe { CFGetTypeID(value.0) } != self.call(|| unsafe { AXValueGetTypeID() })? {
            return None;
        }
        // SAFETY: Callers supply storage with the layout of `kind`.
        self.call(|| unsafe { AXValueGetValue(value.0, kind, out) })?
            .then_some(())
    }

    fn range(&self, element: &CfObject, name: &CStr) -> Option<CfRange> {
        let value = self.attribute(element, name)?;
        let mut range = CfRange::default();
        self.value(
            &value,
            AX_VALUE_CF_RANGE,
            (&mut range as *mut CfRange).cast(),
        )?;
        Some(range)
    }

    fn frame(&self, element: &CfObject) -> Option<ScreenRect> {
        let mut origin = [0.0_f64; 2];
        let mut size = [0.0_f64; 2];
        let position = self.attribute(element, c"AXPosition")?;
        self.value(&position, AX_VALUE_CG_POINT, origin.as_mut_ptr().cast())?;
        let dimensions = self.attribute(element, c"AXSize")?;
        self.value(&dimensions, AX_VALUE_CG_SIZE, size.as_mut_ptr().cast())?;
        Some(ScreenRect {
            x: origin[0],
            y: origin[1],
            width: size[0],
            height: size[1],
        })
    }

    fn range_parameter(&self, range: CfRange) -> Option<CfObject> {
        // SAFETY: AXValueCreate copies the CFRange and returns a retained value.
        let value = self.call(|| unsafe {
            AXValueCreate(AX_VALUE_CF_RANGE, (&range as *const CfRange).cast())
        })?;
        (!value.is_null()).then(|| CfObject(value))
    }

    fn parameterized(
        &self,
        element: &CfObject,
        name: &CStr,
        parameter: &CfObject,
    ) -> Option<CfObject> {
        let attribute = CfObject(cf_string_literal(name)?);
        let mut value = ptr::null();
        // SAFETY: Each argument remains retained for this call.
        let status = self.call(|| unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                element.0,
                attribute.0,
                parameter.0,
                &mut value,
            )
        })?;
        (status == 0 && !value.is_null()).then(|| CfObject(value))
    }

    fn parameterized_rect(
        &self,
        element: &CfObject,
        name: &CStr,
        parameter: &CfObject,
    ) -> Option<ScreenRect> {
        let value = self.parameterized(element, name, parameter)?;
        let mut rect = [0.0_f64; 4];
        self.value(&value, AX_VALUE_CG_RECT, rect.as_mut_ptr().cast())?;
        Some(ScreenRect {
            x: rect[0],
            y: rect[1],
            width: rect[2],
            height: rect[3],
        })
    }

    fn bounds(&self, element: &CfObject, range: CfRange) -> Option<ScreenRect> {
        self.parameterized_rect(element, c"AXBoundsForRange", &self.range_parameter(range)?)
    }

    fn string_for_range(&self, element: &CfObject, range: CfRange) -> Option<String> {
        let value =
            self.parameterized(element, c"AXStringForRange", &self.range_parameter(range)?)?;
        cf_string(value.0).filter(|text| !text.is_empty())
    }
}

fn copy_string(element: &CfObject, name: &CStr) -> Option<String> {
    let value = copy_attribute(element, name)?;
    cf_string(value.0)
}

fn copy_url(element: &CfObject, name: &CStr) -> Option<String> {
    let value = copy_attribute(element, name)?;
    if unsafe { CFGetTypeID(value.0) } != unsafe { CFURLGetTypeID() } {
        return cf_string(value.0);
    }
    // SAFETY: CFURLGetString follows the Get rule; `value` keeps it alive.
    let string = unsafe { CFURLGetString(value.0) };
    (!string.is_null()).then(|| cf_string(string)).flatten()
}

fn cf_string_literal(value: &CStr) -> Option<CfStringRef> {
    let string =
        unsafe { CFStringCreateWithCString(ptr::null(), value.as_ptr(), CF_STRING_ENCODING_UTF8) };
    (!string.is_null()).then_some(string)
}

fn cf_string(value: CfTypeRef) -> Option<String> {
    if unsafe { CFGetTypeID(value) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    let value = value.cast();
    let length = unsafe { CFStringGetLength(value) };
    let capacity = unsafe { CFStringGetMaximumSizeForEncoding(length, CF_STRING_ENCODING_UTF8) }
        .saturating_add(1);
    let mut bytes = vec![0_u8; capacity as usize];
    if !unsafe {
        CFStringGetCString(
            value,
            bytes.as_mut_ptr().cast(),
            capacity,
            CF_STRING_ENCODING_UTF8,
        )
    } {
        return None;
    }
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    bytes.truncate(length);
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_attribute_is_none() {
        assert_eq!(focused_window_title(i32::MAX), None);
        assert_eq!(focused_web_url(i32::MAX), None);
        assert_eq!(char_before_caret(i32::MAX), None);
        assert!(caret_focus(i32::MAX, &|| true).is_none());
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> ScreenRect {
        ScreenRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn only_a_believable_caret_inside_the_window_counts() {
        let window = rect(100.0, 100.0, 800.0, 600.0);
        assert!(plausible_caret(rect(284.4, 366.0, 0.0, 14.0), window));
        assert!(plausible_caret(rect(300.0, 400.0, 2.0, 22.0), window));
        // Chromium's unanswered query on a 1440-point-tall primary display.
        assert!(!plausible_caret(rect(0.0, 1440.0, 0.0, 0.0), window));
        assert!(!plausible_caret(rect(0.0, 0.0, 0.0, 0.0), window));
        // A selection's rect, or a whole field, is no caret.
        assert!(!plausible_caret(rect(200.0, 200.0, 300.0, 18.0), window));
        // Elsewhere on screen.
        assert!(!plausible_caret(rect(1200.0, 300.0, 0.0, 14.0), window));
        assert!(!plausible_caret(rect(f64::NAN, 300.0, 0.0, 14.0), window));
    }

    #[test]
    fn words_are_counted_in_utf16_units() {
        let words: Vec<_> = utf16_words(" so 👋 the  plan\n").collect();
        assert_eq!(words, vec![(1, 2), (4, 2), (7, 3), (12, 4)]);
        assert_eq!(utf16_words("   ").count(), 0);
        assert_eq!(utf16_words("one").collect::<Vec<_>>(), vec![(0, 3)]);
    }
}
