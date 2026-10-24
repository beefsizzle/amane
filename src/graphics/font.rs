use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use fontconfig::{CharSet, Fontconfig, Pattern, UnicodeCoverage};
use swash::FontRef;
use swash::scale::ScaleContext;

use crate::Weight;

// asked in this order for letters the text's own font lacks, like japanese
const FALLBACK_FAMILIES: [&str; 2] = ["Noto Sans", "Noto Sans CJK JP"];

static DEFAULT_FAMILY: LazyLock<Mutex<String>> =
    LazyLock::new(|| Mutex::new(String::from("sans-serif")));

static LOADED: LazyLock<Mutex<HashMap<String, FontRef<'static>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// each font file once, by path: a variable font is one file for every weight
static FILES: LazyLock<Mutex<HashMap<String, &'static [u8]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// a variable font's slider positions, kept by the font's key; a font without any isn't listed
static COORDS: LazyLock<Mutex<HashMap<u64, &'static [i16]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// a letter and the weight of the text it is in
type FallbackKey = (char, u16);

// the font found for each letter at each weight, none when no installed font has it
static FALLBACKS: LazyLock<Mutex<HashMap<FallbackKey, Option<FontRef<'static>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    // swash keeps its working memory here, so each letter doesn't allocate it again
    static CONTEXT: RefCell<ScaleContext> = RefCell::new(ScaleContext::new());
}

pub fn set_default(family: &str) {
    let mut default_family = DEFAULT_FAMILY
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    *default_family = String::from(family);
}

pub fn load_weighted(family: Option<&str>, weight: Weight) -> FontRef<'static> {
    let family = match family {
        Some(family) => String::from(family),
        None => DEFAULT_FAMILY
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    };

    // bold and regular are different files, so each weight is kept apart
    let key = format!("{family} {}", weight.style());

    let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(font) = loaded.get(&key) {
        return *font;
    }

    let font = read(&family, weight);

    loaded.insert(key, font);

    font
}

fn read(family: &str, weight: Weight) -> FontRef<'static> {
    let fontconfig = Fontconfig::new().expect("failed to start fontconfig");

    // regular asks for no style, so plain text finds the same file it always did
    let style = match weight {
        Weight::Regular => None,
        other => Some(other.style()),
    };

    let found = fontconfig.find(family, style).expect("failed to find font");

    // a .ttc file holds several fonts, the index says which one
    let index = found.index.unwrap_or(0) as u32;

    open(&found.path.to_string_lossy(), index)
}

/*
 * caches each font file for the process lifetime, mapped when possible so
 * only the parts used take memory, and read into memory otherwise
 */
fn file(path: &str) -> &'static [u8] {
    let mut files = FILES.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(bytes) = files.get(path) {
        return bytes;
    }

    let bytes = map(path)
        .unwrap_or_else(|| Vec::leak(std::fs::read(path).expect("failed to read font file")));

    files.insert(String::from(path), bytes);

    bytes
}

// the whole file mapped read-only and never unmapped; an update replaces the file, so the mapping keeps the old one
fn map(path: &str) -> Option<&'static [u8]> {
    use std::ffi::{c_int, c_void};
    use std::os::fd::AsRawFd;

    const PROT_READ: c_int = 1;
    const MAP_PRIVATE: c_int = 2;

    unsafe extern "C" {
        fn mmap(
            address: *mut c_void,
            length: usize,
            protection: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
    }

    let file = std::fs::File::open(path).ok()?;

    let length = usize::try_from(file.metadata().ok()?.len())
        .ok()
        .filter(|length| *length > 0)?;

    let address = unsafe {
        mmap(
            std::ptr::null_mut(),
            length,
            PROT_READ,
            MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };

    // MAP_FAILED
    if address as isize == -1 {
        return None;
    }

    // read-only, never unmapped, and nothing else writes to it
    Some(unsafe { std::slice::from_raw_parts(address.cast::<u8>(), length) })
}

fn open(path: &str, index: u32) -> FontRef<'static> {
    let bytes = file(path);

    // fontconfig keeps a variable font's named style in the upper half, the file only knows the lower
    let font_index = index & 0xffff;

    let font = FontRef::from_index(bytes, font_index as usize).expect("failed to parse font");

    // the upper half counts named styles from 1, 0 keeps the font's default
    let instance = index >> 16;

    if instance == 0 {
        return font;
    }

    let Some(style) = font.instances().nth(instance as usize - 1) else {
        return font;
    };

    let coords: Vec<i16> = style.normalized_coords().collect();

    let mut all_coords = COORDS.lock().unwrap_or_else(PoisonError::into_inner);

    all_coords.insert(font.key.value(), Vec::leak(coords));

    font
}

/*
 * where a variable font's sliders sit, like its weight;
 * empty means the font's default, which is all a plain font has
 */
pub fn coords(font: FontRef<'static>) -> &'static [i16] {
    let all_coords = COORDS.lock().unwrap_or_else(PoisonError::into_inner);

    all_coords.get(&font.key.value()).copied().unwrap_or(&[])
}

// the font that has this letter: the text's own font first, then a fallback
pub fn with_letter(font: FontRef<'static>, letter: char) -> (FontRef<'static>, u16) {
    // glyph 0 is the font's placeholder box, which means it lacks the letter
    let id = font.charmap().map(letter);

    if id != 0 {
        return (font, id);
    }

    // so a letter in bold text gets a bold fallback
    let weight = font.attributes().weight().0;

    // a letter no font has draws as the text's own placeholder box
    let Some(fallback) = fallback(letter, weight) else {
        return (font, 0);
    };

    let id = fallback.charmap().map(letter);

    (fallback, id)
}

// swash's working memory for turning letters into outlines and pixels
pub fn with_context<T>(work: impl FnOnce(&mut ScaleContext) -> T) -> T {
    CONTEXT.with_borrow_mut(work)
}

// one letter at a text size: the font that has it, its glyph, and how it measures in pixels
pub struct MeasuredLetter {
    pub font: FontRef<'static>,
    pub id: u16,

    // font units to pixels; a fallback font can measure in other units than the text's own
    pub units: f32,

    // how far the letter moves the pen
    pub advance: f32,
}

pub fn measure_letter(font: FontRef<'static>, letter: char, size: f32) -> MeasuredLetter {
    let (font, id) = with_letter(font, letter);

    let units_per_em = font.metrics(coords(font)).units_per_em;

    let units = size / f32::from(units_per_em);

    let advance = font.glyph_metrics(coords(font)).advance_width(id);

    MeasuredLetter {
        font,
        id,
        units,
        advance: advance * units,
    }
}

fn fallback(letter: char, weight: u16) -> Option<FontRef<'static>> {
    let key = (letter, weight);

    let mut fallbacks = FALLBACKS.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(found) = fallbacks.get(&key) {
        return *found;
    }

    let found = find_covering(letter, weight);

    fallbacks.insert(key, found);

    found
}

/*
 * fontconfig ranks having the letter above the family name, so any font
 * with it ties with noto; the first noto font that has it wins, any other
 * font with it is kept in case no noto font has the letter
 */
fn find_covering(letter: char, weight: u16) -> Option<FontRef<'static>> {
    let fontconfig = Fontconfig::new()?;

    let mut pattern = Pattern::new(&fontconfig).ok()?;

    for family in FALLBACK_FAMILIES {
        let family = std::ffi::CString::new(family).ok()?;

        pattern.add_string(c"family", &family).ok()?;
    }

    pattern
        .add_integer(c"weight", fontconfig_weight(weight))
        .ok()?;

    let mut charset = CharSet::new(&fontconfig).ok()?;

    charset.add_char(letter).ok()?;

    pattern.add_charset(charset).ok()?;

    let candidates = pattern.sort_fonts(UnicodeCoverage::Trim).ok()?;

    let mut other = None;

    for candidate in candidates.iter() {
        let Ok(family) = candidate.get_string(c"family") else {
            continue;
        };

        let Ok(path) = candidate.filename() else {
            continue;
        };

        let index = candidate.face_index().unwrap_or(0) as u32;

        let noto = family.starts_with("Noto");

        // opening a font file is slow, so a second non-noto font isn't tried
        if !noto && other.is_some() {
            continue;
        }

        let font = load_file(path, index);

        // the fonts with the letter come first, so the first without it ends the search
        if font.charmap().map(letter) == 0 {
            break;
        }

        if noto {
            return Some(font);
        }

        other = Some(font);
    }

    other
}

/*
 * fonts count weight from 100 to 900, fontconfig has its own scale
 * where regular is 80 and bold is 200
 */
fn fontconfig_weight(weight: u16) -> i32 {
    let hundreds = (weight + 50) / 100;

    match hundreds {
        0 | 1 => 0,
        2 => 40,
        3 => 50,
        4 => 80,
        5 => 100,
        6 => 180,
        7 => 200,
        8 => 205,
        _ => 210,
    }
}

fn load_file(path: &str, index: u32) -> FontRef<'static> {
    let key = format!("{path} {index}");

    let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(font) = loaded.get(&key) {
        return *font;
    }

    let font = open(path, index);

    loaded.insert(key, font);

    font
}
