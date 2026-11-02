//! A live dictation owns only the text it has confirmed in one focused field.

use std::thread;
use std::time::{Duration, Instant};

use super::*;

const MAX_BYTES: usize = 64 * 1024;
const CONFIRM_FOR: Duration = Duration::from_millis(150);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementIsAttributeSettable(
        element: AxUiElementRef,
        attribute: CfStringRef,
        settable: *mut u8,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CfStringRef;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Range {
    start: usize,
    len: usize,
}

impl Range {
    fn cf(self) -> Result<CfRange, String> {
        Ok(CfRange {
            location: isize::try_from(self.start).map_err(|_| "The text range is too large")?,
            length: isize::try_from(self.len).map_err(|_| "The text range is too large")?,
        })
    }

    fn from_cf(value: CfRange) -> Result<Self, String> {
        Ok(Self {
            start: usize::try_from(value.location).map_err(|_| "The text cursor is unavailable")?,
            len: usize::try_from(value.length).map_err(|_| "The text cursor is unavailable")?,
        })
    }
}

struct Snapshot {
    selection: Range,
    text: String,
}

trait Editor {
    fn snapshot(&mut self, owned: Range, allowed: &dyn Fn() -> bool) -> Result<Snapshot, String>;
    fn write(
        &mut self,
        selection: Range,
        expected: &str,
        text: &str,
        allowed: &dyn Fn() -> bool,
    ) -> Result<(), String>;
}

struct Lease<E> {
    editor: E,
    start: usize,
    inserted: String,
    touched: bool,
    uncertain: bool,
}

impl<E: Editor> Lease<E> {
    fn begin(editor: E, start: usize, allowed: &dyn Fn() -> bool) -> Result<Self, String> {
        let mut lease = Self {
            editor,
            start,
            inserted: String::new(),
            touched: false,
            uncertain: false,
        };
        lease.verify(allowed)?;
        Ok(lease)
    }

    fn owned(&self) -> Range {
        Range {
            start: self.start,
            len: self.inserted.encode_utf16().count(),
        }
    }

    fn verify(&mut self, allowed: &dyn Fn() -> bool) -> Result<(), String> {
        if self.uncertain {
            return Err("The previous live insertion could not be confirmed".into());
        }
        self.verify_text(&self.inserted.clone(), allowed)
    }

    fn verify_text(&mut self, expected: &str, allowed: &dyn Fn() -> bool) -> Result<(), String> {
        if !allowed() {
            return Err("Live insertion is no longer allowed".into());
        }
        let owned = Range {
            start: self.start,
            len: expected.encode_utf16().count(),
        };
        let end = owned
            .start
            .checked_add(owned.len)
            .ok_or("The text range is too large")?;
        let snapshot = self.editor.snapshot(owned, allowed)?;
        if snapshot.selection != (Range { start: end, len: 0 }) {
            return Err("The text cursor moved".into());
        }
        if snapshot.text != expected {
            return Err("The live text was edited".into());
        }
        if !allowed() {
            return Err("Live insertion is no longer allowed".into());
        }
        Ok(())
    }

    fn append(&mut self, delta: &str, allowed: &dyn Fn() -> bool) -> Result<(), String> {
        self.verify(allowed)?;
        if delta.is_empty() {
            return Ok(());
        }
        if self.inserted.len().saturating_add(delta.len()) > MAX_BYTES {
            return Err("The live dictation is too long".into());
        }
        let mut next = self.inserted.clone();
        next.push_str(delta);
        let owned = self.owned();
        let selection = Range {
            start: owned
                .start
                .checked_add(owned.len)
                .ok_or("The text range is too large")?,
            len: 0,
        };
        self.change(selection, "", delta, next, allowed)
    }

    fn replace(&mut self, final_text: &str, allowed: &dyn Fn() -> bool) -> Result<(), String> {
        self.verify(allowed)?;
        if final_text.len() > MAX_BYTES {
            return Err("The live dictation is too long".into());
        }
        if final_text == self.inserted {
            return Ok(());
        }
        let previous = self.inserted.clone();
        self.change(
            self.owned(),
            &previous,
            final_text,
            final_text.to_string(),
            allowed,
        )
    }

    fn change(
        &mut self,
        selection: Range,
        expected: &str,
        text: &str,
        next: String,
        allowed: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        if next.len() > MAX_BYTES {
            return Err("The live dictation is too long".into());
        }
        selection.cf()?;
        // Even a failed native write may have delivered text. An uncertain
        // lease must never fall back to another complete paste or delete it.
        self.touched = true;
        self.uncertain = true;
        self.editor.write(selection, expected, text, allowed)?;
        let deadline = Instant::now() + CONFIRM_FOR;
        let confirm_allowed = || allowed() && Instant::now() < deadline;
        loop {
            if self.verify_text(&next, &confirm_allowed).is_ok() {
                self.inserted = next;
                self.uncertain = false;
                return Ok(());
            }
            if !confirm_allowed() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Err("The app did not confirm the live insertion".into())
    }
}

struct AxEditor {
    pid: i32,
    focus: CaretFocus,
    direct: bool,
}

impl AxEditor {
    fn begin(pid: i32, allowed: &dyn Fn() -> bool) -> Result<(Self, usize), String> {
        let focus =
            caret_focus(pid, &|| allowed()).ok_or("The focused text field is unavailable")?;
        let read = CaretRead { allowed };
        let role = read.string(&focus.focused, c"AXRole");
        if !matches!(
            role.as_deref(),
            Some("AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField")
        ) {
            return Err("The focused control is not an editable text field".into());
        }
        let selection = read
            .range(&focus.focused, c"AXSelectedTextRange")
            .ok_or_else(|| "The text cursor is unavailable".to_string())
            .and_then(Range::from_cf)?;
        if selection.len != 0 {
            return Err("Live insertion waits for an empty selection".into());
        }
        if !settable(&focus.focused, c"AXSelectedTextRange", &read) {
            return Err("The app cannot select its inserted text".into());
        }
        let direct = settable(&focus.focused, c"AXSelectedText", &read);
        Ok((Self { pid, focus, direct }, selection.start))
    }

    fn current(&self, allowed: &dyn Fn() -> bool) -> Result<(), String> {
        if self.focus.is_current(self.pid, &|| allowed()) {
            Ok(())
        } else {
            Err("The focused text field changed".into())
        }
    }
}

impl Editor for AxEditor {
    fn snapshot(&mut self, owned: Range, allowed: &dyn Fn() -> bool) -> Result<Snapshot, String> {
        self.current(allowed)?;
        let read = CaretRead { allowed };
        let selection = read
            .range(&self.focus.focused, c"AXSelectedTextRange")
            .ok_or_else(|| "The text cursor is unavailable".to_string())
            .and_then(Range::from_cf)?;
        let text = slice(&self.focus.focused, owned, &read)
            .ok_or("The app cannot confirm its inserted text")?;
        self.current(allowed)?;
        allowed()
            .then_some(Snapshot { selection, text })
            .ok_or_else(|| "Live insertion is no longer allowed".into())
    }

    fn write(
        &mut self,
        selection: Range,
        expected: &str,
        text: &str,
        allowed: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        self.current(allowed)?;
        let read = CaretRead { allowed };
        let before = read
            .range(&self.focus.focused, c"AXSelectedTextRange")
            .ok_or_else(|| "The text cursor is unavailable".to_string())
            .and_then(Range::from_cf)?;
        let expected_end = selection
            .start
            .checked_add(selection.len)
            .ok_or("The text range is too large")?;
        if before
            != (Range {
                start: expected_end,
                len: 0,
            })
        {
            return Err("The text cursor moved".into());
        }
        if selection.len > 0 {
            if slice(&self.focus.focused, selection, &read).as_deref() != Some(expected) {
                return Err("The live text was edited".into());
            }
            let parameter = read
                .range_parameter(selection.cf()?)
                .ok_or("The text selection is unavailable")?;
            set(
                &self.focus.focused,
                c"AXSelectedTextRange",
                &parameter,
                &read,
            )?;
        }
        self.current(allowed)?;
        let actual = read
            .range(&self.focus.focused, c"AXSelectedTextRange")
            .ok_or_else(|| "The text cursor is unavailable".to_string())
            .and_then(Range::from_cf)?;
        if actual != selection
            || slice(&self.focus.focused, selection, &read).as_deref() != Some(expected)
        {
            return Err("The text cursor or live text changed".into());
        }
        if self.direct {
            let value = read
                .call(|| unsafe {
                    CFStringCreateWithBytes(
                        ptr::null(),
                        text.as_ptr(),
                        text.len() as isize,
                        CF_STRING_ENCODING_UTF8,
                        0,
                    )
                })
                .filter(|value| !value.is_null())
                .map(CfObject)
                .ok_or("Could not encode the live text")?;
            set(&self.focus.focused, c"AXSelectedText", &value, &read)
        } else {
            self.current(allowed)?;
            if !allowed() {
                return Err("Live insertion is no longer allowed".into());
            }
            if text.is_empty() {
                if selection.len == 0 {
                    return Ok(());
                }
                crate::keyboard::delete_selected_text().map_err(|error| error.to_string())
            } else {
                let checked_focus_at = std::cell::Cell::new(Instant::now());
                let check = || {
                    if !allowed() {
                        return false;
                    }
                    if checked_focus_at.get().elapsed() >= Duration::from_millis(50) {
                        if self.current(allowed).is_err() {
                            return false;
                        }
                        checked_focus_at.set(Instant::now());
                    }
                    allowed()
                };
                crate::keyboard::type_text_guarded(text, &check).map_err(|error| error.to_string())
            }
        }
    }
}

fn settable(element: &CfObject, name: &CStr, read: &CaretRead<'_>) -> bool {
    let Some(attribute) = cf_string_literal(name).map(CfObject) else {
        return false;
    };
    let mut writable = 0;
    // SAFETY: The element, attribute, and result storage are live for the call.
    read.call(|| unsafe { AXUIElementIsAttributeSettable(element.0, attribute.0, &mut writable) })
        == Some(0)
        && writable != 0
}

fn set(
    element: &CfObject,
    name: &CStr,
    value: &CfObject,
    read: &CaretRead<'_>,
) -> Result<(), String> {
    let attribute = cf_string_literal(name)
        .map(CfObject)
        .ok_or("The text attribute is unavailable")?;
    // SAFETY: All arguments are retained Core Foundation objects.
    if read.call(|| unsafe { AXUIElementSetAttributeValue(element.0, attribute.0, value.0) })
        == Some(0)
    {
        Ok(())
    } else {
        Err("The app rejected the live text update".into())
    }
}

fn bounded_string(value: &CfObject, read: &CaretRead<'_>) -> Option<String> {
    if read.call(|| unsafe { CFGetTypeID(value.0) })?
        != read.call(|| unsafe { CFStringGetTypeID() })?
    {
        return None;
    }
    let units = read.call(|| unsafe { CFStringGetLength(value.0) })?;
    if units < 0 || units as usize > MAX_BYTES {
        return None;
    }
    let text = read.call(|| cf_string(value.0))??;
    (text.len() <= MAX_BYTES).then_some(text)
}

fn slice(element: &CfObject, range: Range, read: &CaretRead<'_>) -> Option<String> {
    let parameter = read.range_parameter(range.cf().ok()?)?;
    if let Some(value) = read.parameterized(element, c"AXStringForRange", &parameter)
        && let Some(text) = bounded_string(&value, read)
    {
        return Some(text);
    }
    let value = read.attribute(element, c"AXValue")?;
    slice_utf16(&bounded_string(&value, read)?, range)
}

fn slice_utf16(text: &str, range: Range) -> Option<String> {
    let units: Vec<_> = text.encode_utf16().collect();
    String::from_utf16(units.get(range.start..range.start.checked_add(range.len)?)?).ok()
}

pub(crate) struct LiveInsertion {
    lease: Lease<AxEditor>,
}

impl LiveInsertion {
    pub(crate) fn begin(pid: i32, allowed: &impl Fn() -> bool) -> Result<Self, String> {
        let (editor, start) = AxEditor::begin(pid, allowed)?;
        Ok(Self {
            lease: Lease::begin(editor, start, allowed)?,
        })
    }

    pub(crate) fn append(
        &mut self,
        delta: &str,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), String> {
        self.lease.append(delta, allowed)
    }

    pub(crate) fn replace(
        &mut self,
        final_text: &str,
        allowed: &impl Fn() -> bool,
    ) -> Result<(), String> {
        self.lease.replace(final_text, allowed)
    }

    pub(crate) fn rollback(&mut self, allowed: &impl Fn() -> bool) -> Result<(), String> {
        self.lease.replace("", allowed)
    }

    pub(crate) fn inserted(&self) -> &str {
        &self.lease.inserted
    }
    pub(crate) fn touched(&self) -> bool {
        self.lease.touched
    }
    pub(crate) fn pid(&self) -> i32 {
        self.lease.editor.pid
    }

    pub(crate) fn recent_words(
        &self,
        delta: &str,
        allowed: &impl Fn() -> bool,
    ) -> Vec<(String, ScreenRect)> {
        if self.lease.uncertain || !self.lease.editor.focus.is_current(self.pid(), allowed) {
            return Vec::new();
        }
        let Some(rects) = self.lease.editor.focus.words(delta, 14, allowed) else {
            return Vec::new();
        };
        if !self.lease.editor.focus.is_current(self.pid(), allowed) {
            return Vec::new();
        }
        delta
            .split_whitespace()
            .take(14)
            .zip(rects)
            .filter_map(|(word, rect)| {
                rect.filter(|_| word.chars().count() <= 64)
                    .map(|rect| (word.to_string(), rect))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct App {
        text: String,
        caret: usize,
        selected: usize,
        writes: usize,
        unavailable: bool,
        moved_focus: bool,
        fail_after_write: bool,
        hide_after_write: bool,
    }

    struct Mock(Rc<RefCell<App>>);

    impl Editor for Mock {
        fn snapshot(
            &mut self,
            owned: Range,
            allowed: &dyn Fn() -> bool,
        ) -> Result<Snapshot, String> {
            let app = self.0.borrow();
            if !allowed() || app.unavailable || app.moved_focus {
                return Err("unavailable".into());
            }
            Ok(Snapshot {
                selection: Range {
                    start: app.caret,
                    len: app.selected,
                },
                text: slice_utf16(&app.text, owned).ok_or("range unavailable")?,
            })
        }

        fn write(
            &mut self,
            selection: Range,
            expected: &str,
            text: &str,
            allowed: &dyn Fn() -> bool,
        ) -> Result<(), String> {
            if !allowed() {
                return Err("not allowed".into());
            }
            let mut app = self.0.borrow_mut();
            if app.moved_focus || slice_utf16(&app.text, selection).as_deref() != Some(expected) {
                return Err("changed".into());
            }
            let mut units: Vec<_> = app.text.encode_utf16().collect();
            units.splice(
                selection.start..selection.start + selection.len,
                text.encode_utf16(),
            );
            app.text = String::from_utf16(&units).unwrap();
            app.caret = selection.start + text.encode_utf16().count();
            app.selected = 0;
            app.writes += 1;
            if app.hide_after_write {
                app.unavailable = true;
            }
            if app.fail_after_write {
                return Err("delivery uncertain".into());
            }
            Ok(())
        }
    }

    fn app() -> Rc<RefCell<App>> {
        Rc::new(RefCell::new(App {
            text: "before  after".into(),
            caret: 7,
            ..Default::default()
        }))
    }

    #[test]
    fn final_text_replaces_only_the_owned_prefix_once() {
        let app = app();
        let mut lease = Lease::begin(Mock(app.clone()), 7, &|| true).unwrap();
        lease.append("hello", &|| true).unwrap();
        lease.append(" world", &|| true).unwrap();
        lease.replace("Hello, world!", &|| true).unwrap();
        let writes = app.borrow().writes;
        lease.replace("Hello, world!", &|| true).unwrap();
        assert_eq!(app.borrow().text, "before Hello, world! after");
        assert_eq!(app.borrow().writes, writes);
    }

    #[test]
    fn moved_cursor_or_edited_prefix_is_never_replaced_or_rolled_back() {
        for change in 0..3 {
            let app = app();
            let mut lease = Lease::begin(Mock(app.clone()), 7, &|| true).unwrap();
            lease.append("hello", &|| true).unwrap();
            {
                let mut state = app.borrow_mut();
                match change {
                    0 => state.caret = 1,
                    1 => state.text.replace_range(7..8, "H"),
                    _ => state.moved_focus = true,
                }
            }
            let before = app.borrow().text.clone();
            let writes = app.borrow().writes;
            assert!(lease.append(" another word", &|| true).is_err());
            assert!(lease.replace("final", &|| true).is_err());
            assert!(lease.replace("", &|| true).is_err());
            assert_eq!(app.borrow().text, before);
            assert_eq!(app.borrow().writes, writes);
        }
    }

    #[test]
    fn unsupported_metadata_permits_fallback_before_any_write() {
        let app = app();
        app.borrow_mut().unavailable = true;
        assert!(Lease::begin(Mock(app.clone()), 7, &|| true).is_err());
        assert_eq!(app.borrow().writes, 0);
        assert_eq!(app.borrow().text, "before  after");
    }

    #[test]
    fn an_uncertain_write_stays_touched_and_never_overwrites_again() {
        let app = app();
        let mut lease = Lease::begin(Mock(app.clone()), 7, &|| true).unwrap();
        app.borrow_mut().fail_after_write = true;
        assert!(lease.append("hello", &|| true).is_err());
        assert!(lease.touched);
        assert!(lease.replace("final", &|| true).is_err());
        assert!(lease.replace("", &|| true).is_err());
        assert_eq!(app.borrow().writes, 1);
        assert_eq!(app.borrow().text, "before hello after");
    }

    #[test]
    fn an_unconfirmed_write_is_never_repeated_or_rolled_back() {
        let app = app();
        let mut lease = Lease::begin(Mock(app.clone()), 7, &|| true).unwrap();
        app.borrow_mut().hide_after_write = true;
        assert!(lease.append("hello", &|| true).is_err());
        assert!(lease.touched);
        assert!(lease.replace("final", &|| true).is_err());
        assert!(lease.replace("", &|| true).is_err());
        assert_eq!(app.borrow().writes, 1);
        assert_eq!(app.borrow().text, "before hello after");
    }

    #[test]
    fn an_existing_selection_is_never_touched() {
        let app = app();
        app.borrow_mut().selected = 1;
        assert!(Lease::begin(Mock(app.clone()), 7, &|| true).is_err());
        assert_eq!(app.borrow().writes, 0);
        assert_eq!(app.borrow().text, "before  after");
    }

    #[test]
    fn unicode_ranges_and_rollback_preserve_surrounding_text() {
        let app = app();
        let mut lease = Lease::begin(Mock(app.clone()), 7, &|| true).unwrap();
        lease.append("👋 世界", &|| true).unwrap();
        lease.replace("Hello 👋", &|| true).unwrap();
        assert_eq!(app.borrow().text, "before Hello 👋 after");
        lease.replace("", &|| true).unwrap();
        assert_eq!(app.borrow().text, "before  after");
        assert_eq!(app.borrow().caret, 7);
    }
}
