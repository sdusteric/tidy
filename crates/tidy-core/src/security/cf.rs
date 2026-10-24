//! A small CoreFoundation bridge for the Security checks: property lists (XML and binary,
//! through `CFPropertyListCreateWithData`) turned into a plain Rust [`Plist`] value, and the
//! CF helpers the signature checks in [`super::codesign`] share (owned references, strings,
//! file URLs).
//!
//! Only reads. Nothing here writes a plist or changes a preference.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) type CFTypeRef = *const c_void;
pub(crate) type CFStringRef = *const c_void;
pub(crate) type CFDictionaryRef = *const c_void;
pub(crate) type CFArrayRef = *const c_void;
pub(crate) type CFURLRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFDataRef = *const c_void;
type CFErrorRef = *const c_void;
type CFTypeID = usize;
pub(crate) type CFIndex = isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CFRange {
    location: CFIndex,
    length: CFIndex,
}

/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// `kCFNumberSInt64Type`.
const NUMBER_SINT64: CFIndex = 4;
/// `kCFNumberFloat64Type`.
const NUMBER_FLOAT64: CFIndex = 6;
/// `kCFPropertyListImmutable`.
const PLIST_IMMUTABLE: usize = 0;
/// Seconds from 1970-01-01 to 2001-01-01, the CFAbsoluteTime epoch.
const CF_EPOCH_FROM_UNIX: f64 = 978_307_200.0;
/// How deep a plist may nest before the rest is dropped. Launchd plists are two or three
/// levels deep; this only guards against a hostile file.
const MAX_DEPTH: usize = 32;
/// The largest plist file read (launchd plists are a few KB; Info.plist files a few more).
const MAX_PLIST_BYTES: u64 = 4 * 1024 * 1024;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFDictionaryGetTypeID() -> CFTypeID;
    fn CFArrayGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFDataGetTypeID() -> CFTypeID;
    fn CFDateGetTypeID() -> CFTypeID;

    fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external_representation: u8,
    ) -> CFStringRef;
    fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    fn CFStringGetBytes(
        s: CFStringRef,
        range: CFRange,
        encoding: u32,
        loss_byte: u8,
        is_external_representation: u8,
        buffer: *mut u8,
        max_buf_len: CFIndex,
        used_buf_len: *mut CFIndex,
    ) -> CFIndex;

    fn CFDictionaryGetCount(d: CFDictionaryRef) -> CFIndex;
    fn CFDictionaryGetKeysAndValues(
        d: CFDictionaryRef,
        keys: *mut *const c_void,
        values: *mut *const c_void,
    );
    fn CFDictionaryGetValue(d: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFArrayGetCount(a: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, idx: CFIndex) -> *const c_void;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    fn CFNumberIsFloatType(n: CFTypeRef) -> u8;
    fn CFNumberGetValue(n: CFTypeRef, the_type: CFIndex, value: *mut c_void) -> u8;
    fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, len: CFIndex) -> CFDataRef;
    fn CFDataGetLength(d: CFDataRef) -> CFIndex;
    fn CFDataGetBytePtr(d: CFDataRef) -> *const u8;
    fn CFDateGetAbsoluteTime(d: CFTypeRef) -> f64;
    fn CFErrorCopyDescription(e: CFErrorRef) -> CFStringRef;
    fn CFPropertyListCreateWithData(
        alloc: CFAllocatorRef,
        data: CFDataRef,
        options: usize,
        format: *mut CFIndex,
        error: *mut CFErrorRef,
    ) -> CFTypeRef;
    fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const u8,
        buf_len: CFIndex,
        is_directory: u8,
    ) -> CFURLRef;
}

/// One owned CF reference (from a Create or Copy call), released when dropped.
pub(crate) struct Owned(pub(crate) CFTypeRef);

impl Owned {
    pub(crate) fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Create or Copy call, so this code owns one
            // reference to it.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// A new CFString with `text`. Null only when CF is out of memory.
pub(crate) fn cf_string(text: &str) -> Owned {
    let Ok(len) = CFIndex::try_from(text.len()) else {
        return Owned(std::ptr::null());
    };
    // SAFETY: `text` is valid UTF-8 for `len` bytes; a null allocator means the default.
    Owned(unsafe { CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), len, UTF8, 0) })
}

/// A file URL for `path`. Null when the path is too long for CF.
pub(crate) fn cf_file_url(path: &Path) -> Owned {
    let bytes = path.as_os_str().as_bytes();
    let Ok(len) = CFIndex::try_from(bytes.len()) else {
        return Owned(std::ptr::null());
    };
    // SAFETY: `bytes` is valid for `len` bytes; a null allocator means the default.
    Owned(unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), len, 0)
    })
}

/// The text of a CF object when it is a CFString. `cf` may be null.
///
/// # Safety
/// `cf` must be null or a valid CF object for the duration of the call.
pub(crate) unsafe fn string_value(cf: CFTypeRef) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    // SAFETY: the caller passes a valid CF object; the type is checked before use.
    unsafe {
        if CFGetTypeID(cf) != CFStringGetTypeID() {
            return None;
        }
        let len = CFStringGetLength(cf);
        let range = CFRange {
            location: 0,
            length: len,
        };
        let mut needed: CFIndex = 0;
        CFStringGetBytes(cf, range, UTF8, 0, 0, std::ptr::null_mut(), 0, &mut needed);
        let mut buf = vec![0u8; usize::try_from(needed).ok()?];
        let mut used: CFIndex = 0;
        CFStringGetBytes(cf, range, UTF8, 0, 0, buf.as_mut_ptr(), needed, &mut used);
        buf.truncate(usize::try_from(used).ok()?);
        String::from_utf8(buf).ok()
    }
}

/// The value for `key` in a CFDictionary (not retained; valid while `dict` is). Null when
/// `dict` is null, not a dictionary, or has no such key.
///
/// # Safety
/// `dict` must be null or a valid CF object, and `key` a valid CF object.
pub(crate) unsafe fn dict_get(dict: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef {
    if dict.is_null() || key.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes valid objects; the type is checked before the lookup.
    unsafe {
        if CFGetTypeID(dict) != CFDictionaryGetTypeID() {
            return std::ptr::null();
        }
        CFDictionaryGetValue(dict, key)
    }
}

/// The items of a CFArray (not retained; valid while `array` is). Empty when `array` is null
/// or not an array.
///
/// # Safety
/// `array` must be null or a valid CF object.
pub(crate) unsafe fn array_items(array: CFArrayRef) -> Vec<CFTypeRef> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller passes a valid object; the type is checked before use.
    unsafe {
        if CFGetTypeID(array) != CFArrayGetTypeID() {
            return Vec::new();
        }
        (0..CFArrayGetCount(array))
            .map(|i| CFArrayGetValueAtIndex(array, i))
            .collect()
    }
}

/// The value of a CFNumber as an i64. None when `cf` is null or not a number.
///
/// # Safety
/// `cf` must be null or a valid CF object.
pub(crate) unsafe fn number_value(cf: CFTypeRef) -> Option<i64> {
    if cf.is_null() {
        return None;
    }
    // SAFETY: the caller passes a valid object; the type is checked before use.
    unsafe {
        if CFGetTypeID(cf) != CFNumberGetTypeID() {
            return None;
        }
        let mut v: i64 = 0;
        CFNumberGetValue(cf, NUMBER_SINT64, (&mut v as *mut i64).cast());
        Some(v)
    }
}

// ---------- Property lists ----------

/// A parsed property list.
#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    Dict(BTreeMap<String, Plist>),
    Array(Vec<Plist>),
    String(String),
    Bool(bool),
    Integer(i64),
    Real(f64),
    /// Unix seconds.
    Date(f64),
    Data(Vec<u8>),
}

impl Plist {
    /// The value for `key` when this is a dictionary.
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(d) => d.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Plist::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Plist]> {
        match self {
            Plist::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<String, Plist>> {
        match self {
            Plist::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// Unix seconds when this is a date.
    pub fn as_date(&self) -> Option<f64> {
        match self {
            Plist::Date(d) => Some(*d),
            _ => None,
        }
    }
}

/// Parses a property list in any format CF reads (XML, binary, OpenStep). The error is CF's
/// description of what is wrong.
pub fn parse_plist(bytes: &[u8]) -> Result<Plist, String> {
    let len = CFIndex::try_from(bytes.len()).map_err(|_| "the file is too large".to_string())?;
    // SAFETY: `bytes` is valid for `len` bytes; CFDataCreate copies them.
    let data = Owned(unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), len) });
    if data.is_null() {
        return Err("out of memory".into());
    }
    let mut error: CFErrorRef = std::ptr::null();
    let mut format: CFIndex = 0;
    // SAFETY: `data` is a valid CFData; the out-parameters point at locals.
    let plist = Owned(unsafe {
        CFPropertyListCreateWithData(
            std::ptr::null(),
            data.0,
            PLIST_IMMUTABLE,
            &mut format,
            &mut error,
        )
    });
    if plist.is_null() {
        let error = Owned(error);
        let reason = if error.is_null() {
            None
        } else {
            // SAFETY: `error` is the CFError the call created and this code owns.
            let desc = Owned(unsafe { CFErrorCopyDescription(error.0) });
            // SAFETY: `desc` is null or a CFString this code owns.
            unsafe { string_value(desc.0) }
        };
        return Err(reason.unwrap_or_else(|| "not a property list".into()));
    }
    // SAFETY: `plist` is a valid property list object owned above.
    unsafe { convert(plist.0, 0) }.ok_or_else(|| "the property list has no value".to_string())
}

/// Reads and parses the plist at `path`. A file that does not parse is `InvalidData`.
pub fn read_plist(path: &Path) -> io::Result<Plist> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_PLIST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the file is too large for a property list",
        ));
    }
    let bytes = std::fs::read(path)?;
    parse_plist(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Converts one CF property list object. None for a type a plist cannot hold, or past
/// [`MAX_DEPTH`].
///
/// # Safety
/// `cf` must be a valid CF object.
unsafe fn convert(cf: CFTypeRef, depth: usize) -> Option<Plist> {
    if cf.is_null() || depth > MAX_DEPTH {
        return None;
    }
    // SAFETY: `cf` is a valid CF object; each branch checks its type first and only reads
    // values that the object owns for the duration of this call.
    unsafe {
        let t = CFGetTypeID(cf);
        if t == CFStringGetTypeID() {
            return string_value(cf).map(Plist::String);
        }
        if t == CFBooleanGetTypeID() {
            return Some(Plist::Bool(CFBooleanGetValue(cf) != 0));
        }
        if t == CFNumberGetTypeID() {
            if CFNumberIsFloatType(cf) != 0 {
                let mut v: f64 = 0.0;
                CFNumberGetValue(cf, NUMBER_FLOAT64, (&mut v as *mut f64).cast());
                return Some(Plist::Real(v));
            }
            let mut v: i64 = 0;
            CFNumberGetValue(cf, NUMBER_SINT64, (&mut v as *mut i64).cast());
            return Some(Plist::Integer(v));
        }
        if t == CFDateGetTypeID() {
            return Some(Plist::Date(CFDateGetAbsoluteTime(cf) + CF_EPOCH_FROM_UNIX));
        }
        if t == CFDataGetTypeID() {
            let len = usize::try_from(CFDataGetLength(cf)).ok()?;
            let ptr = CFDataGetBytePtr(cf);
            if ptr.is_null() || len == 0 {
                return Some(Plist::Data(Vec::new()));
            }
            return Some(Plist::Data(std::slice::from_raw_parts(ptr, len).to_vec()));
        }
        if t == CFArrayGetTypeID() {
            let items = array_items(cf)
                .into_iter()
                .filter_map(|v| convert(v, depth + 1))
                .collect();
            return Some(Plist::Array(items));
        }
        if t == CFDictionaryGetTypeID() {
            let n = usize::try_from(CFDictionaryGetCount(cf)).ok()?;
            let mut keys = vec![std::ptr::null::<c_void>(); n];
            let mut values = vec![std::ptr::null::<c_void>(); n];
            CFDictionaryGetKeysAndValues(cf, keys.as_mut_ptr(), values.as_mut_ptr());
            let mut map = BTreeMap::new();
            for (k, v) in keys.into_iter().zip(values) {
                let Some(key) = string_value(k) else { continue };
                if let Some(value) = convert(v, depth + 1) {
                    map.insert(key, value);
                }
            }
            return Some(Plist::Dict(map));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.example.agent</string>
    <key>ProgramArguments</key><array><string>/bin/ls</string><string>-l</string></array>
    <key>RunAtLoad</key><true/>
    <key>StartInterval</key><integer>3600</integer>
    <key>Ratio</key><real>0.5</real>
    <key>When</key><date>2001-01-01T00:00:00Z</date>
    <key>Blob</key><data>aGk=</data>
    <key>Nested</key><dict><key>Inner</key><false/></dict>
</dict>
</plist>"#;

    #[test]
    fn xml_plists_convert_every_type() {
        let p = parse_plist(XML.as_bytes()).unwrap();
        assert_eq!(
            p.get("Label").and_then(Plist::as_str),
            Some("com.example.agent")
        );
        let args = p.get("ProgramArguments").and_then(Plist::as_array).unwrap();
        assert_eq!(args[0].as_str(), Some("/bin/ls"));
        assert_eq!(args.len(), 2);
        assert_eq!(p.get("RunAtLoad").and_then(Plist::as_bool), Some(true));
        assert_eq!(p.get("StartInterval"), Some(&Plist::Integer(3600)));
        assert_eq!(p.get("Ratio"), Some(&Plist::Real(0.5)));
        assert_eq!(
            p.get("When").and_then(Plist::as_date),
            Some(CF_EPOCH_FROM_UNIX)
        );
        assert_eq!(p.get("Blob"), Some(&Plist::Data(b"hi".to_vec())));
        assert_eq!(
            p.get("Nested")
                .and_then(|n| n.get("Inner"))
                .and_then(Plist::as_bool),
            Some(false)
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(parse_plist(b"not a plist at all {{{").is_err());
        assert!(parse_plist(b"").is_err());
        assert!(parse_plist(b"bplist00\x00\x01\x02").is_err());
    }

    #[test]
    fn cf_strings_round_trip_unicode() {
        let s = cf_string("Caf\u{e9} \u{1f600}");
        // SAFETY: `s` is a CFString owned by this test.
        assert_eq!(
            unsafe { string_value(s.0) }.as_deref(),
            Some("Caf\u{e9} \u{1f600}")
        );
        // SAFETY: null is allowed.
        assert_eq!(unsafe { string_value(std::ptr::null()) }, None);
    }
}
