//! Personal data (PII) detection on extracted text.
//!
//! Pattern-based and deterministic: every finding has a type, the exact
//! character span, the matched text and a confidence. Detectors:
//!   email, phone, url / profile link, national IDs (Aadhaar, PAN, SSN),
//!   date of birth, postal code, UPI ID, person name (the page title when it
//!   looks like a name, a "Name:" field, a name after Mr/Ms/Dr, a referee)
//!   and sensitive "Label: value" fields (gender, nationality, address, ...).
//! Addresses are split: the street part is `address`, the PIN or ZIP is
//! `postal_code`, and the city, state and country after them are `location`,
//! which is listed but never redacted (the candidate's city is needed, and
//! alone does not identify anyone). Unlabelled addresses are found only in
//! unmistakable shapes (a street line ending in a PIN, a US street + ZIP).
//! Names in free text need an NER model and are not detected here.

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PiiSpan {
    /// email, phone, url, profile, national_id, date_of_birth, postal_code,
    /// person_name, gender, marital_status, nationality, religion, address,
    /// location (never redacted), upi_id, age
    pub kind: &'static str,
    /// Byte offsets into the block text.
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// 0..1, how sure the detector is.
    pub confidence: f32,
}

struct Detector {
    kind: &'static str,
    re: Regex,
    confidence: f32,
}

fn detectors() -> &'static [Detector] {
    static D: OnceLock<Vec<Detector>> = OnceLock::new();
    D.get_or_init(|| {
        let d = |kind, pattern: &str, confidence| Detector { kind, re: Regex::new(pattern).unwrap(), confidence };
        vec![
            d("email", r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}\b", 0.99),
            d("profile", r"(?i)\b(?:https?://)?(?:www\.)?(?:linkedin\.com/in/|github\.com/|gitlab\.com/|twitter\.com/|x\.com/|behance\.net/|dribbble\.com/|medium\.com/@|stackoverflow\.com/users/)[A-Za-z0-9_\-./%]+", 0.95),
            d("url", r"(?i)\b(?:https?://|www\.)[^\s|<>]+", 0.8),
            // Bare lowercase domains (portfolio sites). Case-sensitive so that
            // terms like "ASP.NET" or "Node.js" are not taken for domains.
            d("url", r"\b[a-z0-9][a-z0-9\-]*(?:\.[a-z0-9\-]+)*\.(?:com|dev|io|me|ai|app|in|co|org|net|site|xyz|tech|page)(?:/[^\s|<>]*)?\b", 0.7),
            // Addresses with a space inside, from a line wrap or a text
            // layer: "jane@mail. com", "jane@mail .com". Before a space the
            // domain has no dot of its own, so "jane@mail.com. In 2020" is
            // the address and a sentence, not "jane@mail.com.in".
            d("email", r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9\-]+\.\s(?:com|in|org|net|co|io|edu|ac|gov|me|ai|uk|us)\b", 0.95),
            d("email", r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9\-]+(?:\.[a-z0-9\-]+)*\s\.(?:com|in|org|net|co|io|edu|ac|gov|me|ai|uk|us)\b", 0.95),
            // "jane @mail.com", "jane@ mail.com". Prose can read like this
            // ("team @ acme.com"), so it is only sure next to an "Email:"
            // label (see `detect`); alone it is listed for confirmation.
            d("email", r"(?i)\b[a-z0-9._%+\-]+(?:\s@\s?|@\s)[a-z0-9\-]+(?:\.[a-z0-9\-]+)*\.[a-z]{2,}\b", 0.75),
            // ID numbers. Each pattern is confirmed by `valid_id` (check
            // digits, structure), so look-alike numbers are not removed.
            d("national_id", r"\b[2-9]\d{3}[\s\-]?\d{4}[\s\-]?\d{4}\b", 0.9),          // Aadhaar (Verhoeff)
            d("national_id", r"\b[A-Z]{3}[ABCFGHJLPT][A-Z]\d{4}[A-Z]\b", 0.95),           // PAN
            d("national_id", r"\b\d{3}[\-\s]\d{2}[\-\s]\d{4}\b", 0.9),                  // US SSN
            d("national_id", r"\b[A-Z]{3}\d{7}\b", 0.7),                                  // Indian voter ID (EPIC)
            d("driving_licence", r"\b[A-Z]{2}[\s\-]?\d{2}[\s\-]?(?:19|20)\d{2}[\s\-]?\d{7}\b", 0.95), // Indian DL
            d("card_number", r"\b[3-6]\d{3}(?:[\s\-]?\d{3,4}){3}(?:[\s\-]?\d{1,3})?\b", 0.95), // Luhn
            d("bank_account", r"\b[A-Z]{2}\d{2}(?:\s?[A-Z0-9]{4}){2,7}(?:\s?[A-Z0-9]{1,3})?\b", 0.95), // IBAN (mod 97)
            // +country code or local number: 8-15 digits with spaces, dots,
            // dashes or brackets. Year ranges ("2019 - 2023") are excluded below.
            // The area code's "(" is optional: text layers lose it ("080) 4123 4567").
            // Groups are joined by a space, a dot or a dash, the dash
            // possibly spaced ("+1 (555) 123 - 4567", "+91 – 98765 43210").
            d("phone", r"(?:\+\d{1,3}(?:\s?[.\-–]\s?|\s)?)?(?:\(?\d{1,4}\)(?:\s?[.\-–]\s?|\s)?)?\d{1,5}(?:(?:\s?[.\-–]\s?|\s)?\d{2,5}){1,5}", 0.85),
            d("date_of_birth", r"(?i)\b(?:d\.?o\.?b\.?|date\s+of\s+birth|born(?:\s+on)?)\s*[:\-]?\s*[0-9A-Za-z ,./\-]{6,20}", 0.95),
            d("postal_code", r"(?i)\b(?:pin(?:\s*code)?|zip(?:\s*code)?|postal\s*code)\s*[:\-]?\s*\d{5,6}\b", 0.9),
            // UPI IDs: a name at a bank's payment handle, with no domain
            // ending ("ravi@okhdfc"; "ravi@paytm.com" is an email).
            d("upi_id", r"(?i)\b[a-z0-9][a-z0-9._\-]{1,63}@(?:okhdfcbank|okhdfc|oksbi|okicici|okaxis|ybl|ibl|axl|paytm|upi|apl|yapl|ptyes|ptsbi|pthdfc|ptaxis|axisbank|icici|sbi|hdfcbank|kotak|barodampay|aubank|freecharge|fbl|idfcbank|indus|rbl|yesbank|airtel|jio|slc|amazonpay|ikwik|waicici|wahdfcbank|wasbi|naviaxis|superyes)\b", 0.9),
        ]
    })
}

/// "Label: value" fields whose value is personal or sensitive.
fn labelled_field(label: &str) -> Option<(&'static str, f32)> {
    // "PAN No.", "DL #", "Driving Licence Number" also match the plain label.
    let l = label.trim().trim_end_matches(['.', '#', ' ', ':']).to_lowercase();
    let l = l.trim();
    labelled_kind(l).or_else(|| {
        let base = ["no", "number", "num", "id", "#"].iter().find_map(|s| l.strip_suffix(s)).map(str::trim)?;
        labelled_kind(base.trim_end_matches('.').trim())
    })
}

fn labelled_kind(l: &str) -> Option<(&'static str, f32)> {
    Some(match l {
        "name" | "full name" | "candidate name" => ("person_name", 0.95),
        "gender" | "sex" => ("gender", 0.95),
        "marital status" => ("marital_status", 0.95),
        "nationality" | "citizenship" => ("nationality", 0.9),
        "religion" | "caste" => ("religion", 0.95),
        "age" => ("age", 0.9),
        "address" | "permanent address" | "current address" | "present address" | "residential address"
        | "home address" | "postal address" | "mailing address" | "correspondence address" | "residence"
        | "location" => ("address", 0.85),
        "upi" | "upi id" | "vpa" => ("upi_id", 0.95),
        "references" | "reference" | "referees" | "referee" => ("referee", 0.9),
        "father's name" | "fathers name" | "father name" | "mother's name" | "spouse name" => ("person_name", 0.95),
        "passport" | "passport no" | "passport number" | "aadhaar" | "aadhar" | "aadhaar no" | "aadhar no"
        | "aadhaar number" | "aadhar number" | "uid" | "uidai" | "pan" | "pan no" | "pan number" | "pan card"
        | "ssn" | "social security" | "social security number" | "social security no" | "sin" | "nin"
        | "national insurance number" | "ni number" | "national id" | "id number" | "voter id" | "epic no"
        | "uan" | "uan no" | "pf no" | "pf number" | "esic no" | "tax id" | "tin" | "itin" => ("national_id", 0.95),
        "driving licence" | "driving license" | "driver's license" | "drivers license" | "driver license"
        | "dl" | "dl no" | "dl number" | "licence no" | "license no" | "licence number" | "license number" => {
            ("driving_licence", 0.95)
        }
        "bank account" | "account no" | "account number" | "a/c no" | "a/c" | "acc no" | "iban" | "bank a/c" => {
            ("bank_account", 0.95)
        }
        "credit card" | "card no" | "card number" | "debit card" => ("card_number", 0.95),
        "date of birth" | "dob" | "d.o.b" => ("date_of_birth", 0.95),
        "phone" | "mobile" | "mob" | "contact" | "contact no" | "phone no" | "mobile no" | "tel" => ("phone", 0.95),
        "email" | "e-mail" | "mail" => ("email", 0.95),
        "linkedin" | "linkedin profile" | "github" | "gitlab" | "twitter" | "x" | "skype" | "portfolio" | "website"
        | "personal website" | "behance" | "dribbble" => ("profile", 0.9),
        _ => return None,
    })
}

/// Find PII in prose (paragraphs, list items): the pattern detectors plus,
/// with the `ner` feature, people's names and places found by the NER model.
pub fn detect_prose(text: &str) -> Vec<PiiSpan> {
    #[allow(unused_mut)]
    let mut spans = detect(text);
    #[cfg(feature = "ner")]
    {
        for e in crate::ner::entities(text) {
            if spans.iter().any(|s| s.start < e.end && e.start < s.end) {
                continue;
            }
            spans.push(PiiSpan { kind: e.kind, start: e.start, end: e.end, text: text[e.start..e.end].to_string(), confidence: e.score });
        }
        spans.sort_by_key(|s| s.start);
    }
    spans
}

/// Find PII in one block of text.
pub fn detect(text: &str) -> Vec<PiiSpan> {
    let mut out: Vec<PiiSpan> = Vec::new();
    for det in detectors() {
        let found: Vec<(usize, usize)> =
            if det.kind == "phone" { phone_matches(&det.re, text) } else { det.re.find_iter(text).map(|m| (m.start(), m.end())).collect() };
        for (start, end) in found {
            let m = &text[start..end];
            // "2008 2012 2015": a list of years, not a number.
            if matches!(det.kind, "national_id" | "card_number") && year_list(m) {
                continue;
            }
            if matches!(det.kind, "national_id" | "card_number" | "bank_account") && !valid_id(m) {
                continue;
            }
            // A card or ID number is not also a phone number.
            if det.kind == "phone" && out.iter().any(|s| overlaps(s, start, end)) {
                continue;
            }
            if det.kind == "url" && out.iter().any(|s| matches!(s.kind, "profile" | "email" | "url") && overlaps(s, start, end)) {
                continue;
            }
            if det.kind == "upi_id" && text[end..].starts_with('.') && text[end + 1..].starts_with(|c: char| c.is_alphabetic()) {
                continue;
            }
            push(&mut out, det.kind, start, end, text, det.confidence);
        }
    }
    // "Label: value" fields, possibly several on one line separated by "|".
    for part in split_fields(text) {
        let (start, field) = part;
        if let Some(colon) = field.find(':') {
            if let Some((kind, conf)) = labelled_field(&field[..colon]) {
                // The value runs to the next separator (",", ";", "|" or a
                // wide gap), so one label never swallows the next field. An
                // address has commas of its own: it runs on to ";", "|", a
                // wide gap or the next "Label:".
                let rest = &field[colon + 1..];
                let cut = if kind == "address" {
                    address_end(rest)
                } else {
                    rest.char_indices()
                        .find(|&(i, c)| matches!(c, ',' | ';' | '|') || rest[i..].starts_with("   "))
                        .map(|(i, _)| i)
                        .unwrap_or(rest.len())
                };
                let value = rest[..cut].trim();
                if value.is_empty() {
                    continue;
                }
                let vs = start + colon + 1 + rest[..cut].find(value).unwrap_or(0);
                let ve = vs + value.len();
                // "Email:" and "Phone:" vouch for the address or number found
                // in the value; the words after it ("Email: a@b.com Pune")
                // are not part of it, and a value with none ("Phone: on
                // request") is not one.
                if matches!(kind, "email" | "phone") {
                    let mut vouched = false;
                    for s in out.iter_mut().filter(|s| s.kind == kind && vs <= s.start && s.end <= ve) {
                        s.confidence = s.confidence.max(0.99);
                        vouched = true;
                    }
                    // A value that is nothing but a number, in a shape the
                    // pattern does not know, is still the number.
                    let only_number = value.chars().all(|c| c.is_ascii_digit() || " ()+.-–".contains(c));
                    if !vouched && kind == "phone" && only_number && plausible_phone(value) && !year_list(value) {
                        push(&mut out, kind, vs, ve, text, conf);
                    }
                    continue;
                }
                if kind == "address" {
                    out.extend(address_spans(text, vs, ve, conf));
                    continue;
                }
                // A UPI ID is one word.
                if kind == "upi_id" {
                    let id = value.split_whitespace().next().unwrap_or("");
                    if id.contains('@') {
                        push(&mut out, kind, vs, vs + id.len(), text, conf);
                    }
                    continue;
                }
                // "References: Mr. Anil Mehta, Manager" names a referee;
                // "References: available on request" does not.
                if kind == "referee" {
                    let name = value.trim_start_matches(|c: char| !c.is_alphanumeric());
                    let bare = strip_title(name);
                    if looks_like_name(bare) {
                        let ns = vs + value.find(name).unwrap_or(0);
                        push(&mut out, "person_name", ns, ns + name.len(), text, conf);
                    }
                    continue;
                }
                if ve > vs {
                    push(&mut out, kind, vs, ve, text, conf);
                }
            }
        }
    }
    titled_names(text, &mut out);
    unlabelled_addresses(text, &mut out);
    city_pins(text, &mut out);
    // Widest first, then the most confident: a labelled "SSN: 123456789"
    // wins over the phone pattern matching the same digits.
    out.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)).then(b.confidence.total_cmp(&a.confidence)));
    // Drop spans fully inside a more specific earlier span.
    let mut kept: Vec<PiiSpan> = Vec::new();
    for s in out {
        if !kept.iter().any(|k| k.start <= s.start && s.end <= k.end) {
            kept.push(s);
        }
    }
    kept
}

/// Whether a short title-like line is a person's name: 2-4 words, letters
/// only (plus . ' -), each word capitalised or the whole line in capitals.
pub fn looks_like_name(text: &str) -> bool {
    let t = text.trim().trim_start_matches('#').trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    if !(2..=4).contains(&words.len()) || t.chars().count() > 40 {
        return false;
    }
    let all_caps = t.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    words.iter().all(|w| {
        w.chars().all(|c| c.is_alphabetic() || matches!(c, '.' | '\'' | '-'))
            && (all_caps || w.chars().next().is_some_and(|c| c.is_uppercase()))
    }) && !t.to_lowercase().split_whitespace().any(|w| NOT_NAME_WORDS.contains(&w))
}

/// Words that mark a heading as a section or role, not a name.
pub(crate) const NOT_NAME_WORDS: &[&str] = &[
    "resume", "curriculum", "vitae", "cv", "profile", "summary", "experience", "education", "skills",
    "engineer", "developer", "manager", "analyst", "designer", "consultant", "contents", "chapter", "table",
    "introduction", "objective", "projects", "certifications", "contact", "references", "report", "page",
    "about", "personal", "details", "career", "work", "history", "technical", "professional", "key", "core",
    "competencies", "achievements", "languages", "hobbies", "interests", "declaration", "senior", "junior", "lead",
    "architect", "specialist", "executive", "officer", "intern", "student", "fresher", "associate", "director",
    "head", "administrator", "admin", "tester", "scientist", "coordinator", "technician", "accountant", "teacher",
    "email", "phone", "mobile", "address", "linkedin", "github", "portfolio", "website", "highlights", "strengths",
    "employment", "qualifications", "training", "awards", "publications", "overview", "information", "the", "and",
];

/// Details of the person that the caller already knows (from an ATS or a
/// form): they are removed wherever and however they are written.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Known {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    /// Any other values to remove (an address, an ID number, a former name).
    #[serde(default)]
    pub other: Vec<String>,
}

static KNOWN: std::sync::RwLock<Option<Known>> = std::sync::RwLock::new(None);

/// Set (or clear) the known details used by every following analysis.
pub fn set_known(k: Option<Known>) {
    *KNOWN.write().unwrap_or_else(|e| e.into_inner()) = k;
}

pub fn known() -> Option<Known> {
    KNOWN.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Confirm an ID-shaped match: Verhoeff for Aadhaar, Luhn for card
/// numbers, mod 97 for IBANs, valid area/group/serial for US SSNs.
fn valid_id(s: &str) -> bool {
    let compact: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let digits = compact.chars().all(|c| c.is_ascii_digit());
    if compact.starts_with(|c: char| c.is_ascii_uppercase()) {
        if compact.len() == 10 {
            return true; // PAN / voter ID: structure already matched
        }
        return iban_ok(&compact);
    }
    if digits && compact.len() == 12 {
        return verhoeff_ok(&compact);
    }
    if digits && compact.len() == 9 {
        let (a, g, n) = (&compact[..3], &compact[3..5], &compact[5..]);
        return a != "000" && a != "666" && !a.starts_with('9') && g != "00" && n != "0000";
    }
    digits && (13..=19).contains(&compact.len()) && luhn_ok(&compact)
}

fn verhoeff_ok(n: &str) -> bool {
    const D: [[u8; 10]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], [1, 2, 3, 4, 0, 6, 7, 8, 9, 5], [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
        [3, 4, 0, 1, 2, 8, 9, 5, 6, 7], [4, 0, 1, 2, 3, 9, 5, 6, 7, 8], [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
        [6, 5, 9, 8, 7, 1, 0, 4, 3, 2], [7, 6, 5, 9, 8, 2, 1, 0, 4, 3], [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ];
    const P: [[u8; 10]; 8] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], [1, 5, 7, 6, 2, 8, 3, 0, 9, 4], [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
        [8, 9, 1, 6, 0, 4, 3, 5, 2, 7], [9, 4, 5, 3, 1, 2, 6, 8, 7, 0], [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
        [2, 7, 9, 3, 8, 0, 6, 4, 1, 5], [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
    ];
    let mut c = 0u8;
    for (i, ch) in n.bytes().rev().enumerate() {
        c = D[c as usize][P[i % 8][(ch - b'0') as usize] as usize];
    }
    c == 0
}

fn luhn_ok(n: &str) -> bool {
    let sum: u32 = n
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| {
            let d = (b - b'0') as u32;
            if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d }
        })
        .sum();
    sum % 10 == 0
}

fn iban_ok(s: &str) -> bool {
    if !(15..=34).contains(&s.len()) {
        return false;
    }
    let moved = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u64 = 0;
    for c in moved.chars() {
        let v = if c.is_ascii_digit() { c as u64 - '0' as u64 } else { c as u64 - 'A' as u64 + 10 };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

/// Whether every group of digits is a year (1950-2039): "2019 2021 2023".
fn year_list(s: &str) -> bool {
    let groups: Vec<&str> = s.split(|c: char| !c.is_ascii_digit()).filter(|g| !g.is_empty()).collect();
    groups.len() >= 2
        && groups.iter().all(|g| g.len() == 4 && g.parse::<u32>().is_ok_and(|y| (1950..=2039).contains(&y)))
}

/// Phone numbers in `text`. A number never starts inside a word or a link
/// ("…/in/jane-1a7 98765 43210" is the number after the link), and a run of
/// digits longer than any number ("+91 98765 43210 080 4123 4567", two numbers
/// side by side) is cut at its spaces into the numbers it holds.
fn phone_matches(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(m) = re.find_at(text, at) {
        // The word the match starts in: a link or an ID ("12a45", "/in/x-")
        // has digits or "/@=" in it; a label glued on by the text layer
        // ("Mob9876543210", "Ph.9876543210") has not.
        let word = &text[text[..m.start()].rfind(char::is_whitespace).map_or(0, |i| i + 1)..m.start()];
        let glued = word.chars().next_back().is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '=' | '#' | '@'));
        let opens = m.as_str().starts_with(['+', '(']);
        if !opens && glued && word.contains(|c: char| c.is_ascii_digit() || matches!(c, '/' | '@' | '=')) {
            // Skip to the end of the word the match started in.
            at = text[m.start()..].find(char::is_whitespace).map_or(text.len(), |i| m.start() + i);
            continue;
        }
        at = m.end();
        let s = m.as_str();
        if plausible_phone(s) && !year_list(s) {
            out.push((m.start(), m.end()));
            continue;
        }
        // Too many digits: the longest plausible run of whole space-separated
        // groups, from each group in turn.
        let groups: Vec<(usize, usize)> = s
            .split_whitespace()
            .map(|g| {
                let i = g.as_ptr() as usize - s.as_ptr() as usize;
                (m.start() + i, m.start() + i + g.len())
            })
            .collect();
        let mut i = 0;
        while i < groups.len() {
            let hit = (i..groups.len()).rev().find(|&j| {
                let t = &text[groups[i].0..groups[j].1];
                plausible_phone(t) && !year_list(t)
            });
            match hit {
                Some(j) => {
                    out.push((groups[i].0, groups[j].1));
                    i = j + 1;
                }
                None => i += 1,
            }
        }
    }
    out
}

fn plausible_phone(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    if !(8..=15).contains(&digits) {
        return false;
    }
    // Year ranges and dates are not phone numbers.
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let year_range = Regex::new(r"^(19|20)\d{2}[-–](19|20)\d{2}$").unwrap();
    // 2026-11-13, 2026.11.13, 13-11-2026, 13.11.2026
    let date = Regex::new(r"^((19|20)\d{2}[-.]\d{1,2}[-.]\d{1,2}|\d{1,2}[-.]\d{1,2}[-.](19|20)\d{2})$").unwrap();
    !year_range.is_match(&compact) && !date.is_match(&compact) && !compact.contains('/')
}

fn overlaps(s: &PiiSpan, a: usize, b: usize) -> bool {
    s.start < b && a < s.end
}

fn push(out: &mut Vec<PiiSpan>, kind: &'static str, start: usize, end: usize, text: &str, confidence: f32) {
    out.push(PiiSpan { kind, start, end, text: text[start..end].to_string(), confidence });
}

/// Words that mark part of an address as the street, not a place name.
const STREET_WORDS: &[&str] = &[
    "cross", "main", "road", "rd", "street", "st", "lane", "ln", "avenue", "ave", "nagar", "layout", "sector", "flat",
    "floor", "colony", "block", "phase", "apartment", "apartments", "apt", "house", "plot", "marg", "society",
    "tower", "towers", "enclave", "vihar", "extension", "stage", "near", "opp", "opposite", "behind", "suite",
    "unit", "blvd", "boulevard", "drive", "dr", "court", "ct", "way", "place", "pl", "terrace", "parkway", "pkwy",
    "highway", "hwy", "circle", "cir", "residency", "residence", "villa", "villas", "chs", "bldg", "building",
];

fn has_street_word(part: &str) -> bool {
    part.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| {
            let w = w.to_lowercase();
            STREET_WORDS.contains(&w.as_str()) || (w.len() > 6 && w.ends_with("nagar"))
        })
}

fn pin_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Indian PIN (6 digits, first 1-9, sometimes "560 038") or US ZIP(+4).
    R.get_or_init(|| Regex::new(r"\b(?:[1-9]\d{2}\s?\d{3}|\d{5}(?:-\d{4})?)\b").unwrap())
}

/// Where an address value ends: at ";", "|", a wide gap, a date (a job
/// line: "Location: Austin, TX, USA Jan 2020 – Mar 2022") or the next
/// "Label:" ("... Bengaluru 560038  Phone: ...").
fn address_end(rest: &str) -> usize {
    static DATE: OnceLock<Regex> = OnceLock::new();
    let date = DATE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?\s*'?\d{2,4}\b|\b(?:19|20)\d{2}\b").unwrap()
    });
    let hard = rest
        .char_indices()
        .find(|&(i, c)| matches!(c, ';' | '|') || rest[i..].starts_with("   "))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let hard = date.find(&rest[..hard]).map(|m| m.start()).unwrap_or(hard);
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"(?:^|[\s,])([A-Za-z][A-Za-z .'/]{1,30}):").unwrap());
    label
        .captures_iter(&rest[..hard])
        .filter_map(|c| c.get(1))
        .find(|m| labelled_field(m.as_str()).is_some())
        .map(|m| m.start())
        .unwrap_or(hard)
}

/// Split the address in `text[start..end]`: the PIN or ZIP is
/// `postal_code`; up to three place names at the end (city, state,
/// country: no digits, no street word) are `location`; the rest, the
/// house and street, is `address`.
fn address_spans(text: &str, start: usize, end: usize, confidence: f32) -> Vec<PiiSpan> {
    let value = &text[start..end];
    let mut out = Vec::new();
    // The postal code is the last one, so a house number is never taken.
    let pin = pin_re().find_iter(value).last().filter(|m| m.start() > 0);
    if let Some(m) = pin {
        push(&mut out, "postal_code", start + m.start(), start + m.end(), text, confidence.max(0.9));
    }
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut s = 0;
    for (i, c) in value.char_indices() {
        if c == ',' {
            parts.push((s, i));
            s = i + 1;
        }
    }
    parts.push((s, value.len()));
    // A part's text without the postal code and stray dashes.
    let clean = |(a, b): (usize, usize)| -> (usize, usize) {
        let (mut a, mut b) = (a, b);
        if let Some(m) = pin {
            if m.start() >= a && m.end() <= b {
                if value[a..m.start()].trim().is_empty() {
                    a = m.end();
                } else {
                    b = m.start();
                }
            }
        }
        let t = &value[a..b];
        let lead = t.len() - t.trim_start_matches(|c: char| c.is_whitespace() || "-–.".contains(c)).len();
        let t2 = &t[lead..];
        let kept = t2.trim_end_matches(|c: char| c.is_whitespace() || "-–.(".contains(c)).len();
        (a + lead, a + lead + kept)
    };
    let mut place_from = parts.len();
    let mut places = 0;
    for (i, &p) in parts.iter().enumerate().rev() {
        let (a, b) = clean(p);
        let t = &value[a..b];
        if t.is_empty() {
            continue;
        }
        let place = places < 3
            && !t.chars().any(|c| c.is_ascii_digit() || c == ':')
            && t.split_whitespace().count() <= 3
            && t.len() <= 30
            && !has_street_word(t);
        if !place {
            break;
        }
        places += 1;
        place_from = i;
    }
    let street: Vec<(usize, usize)> = parts[..place_from].iter().map(|&p| clean(p)).filter(|(a, b)| b > a).collect();
    if let (Some(first), Some(last)) = (street.first(), street.last()) {
        push(&mut out, "address", start + first.0, start + last.1, text, confidence);
    }
    let place: Vec<(usize, usize)> = parts[place_from..].iter().map(|&p| clean(p)).filter(|(a, b)| b > a).collect();
    if let (Some(first), Some(last)) = (place.first(), place.last()) {
        push(&mut out, "location", start + first.0, start + last.1, text, confidence);
    }
    out
}

/// Addresses without a label, only in shapes that are clearly addresses:
/// a field of three or more comma-separated parts ending in a PIN, that
/// starts with a house number or names a street ("12, 3rd Cross,
/// Indiranagar, Bengaluru 560038"); and a US street address with a state
/// and ZIP ("1600 Elm Street, Springfield, IL 62704").
fn unlabelled_addresses(text: &str, out: &mut Vec<PiiSpan>) {
    static INDIA: OnceLock<Regex> = OnceLock::new();
    static US: OnceLock<Regex> = OnceLock::new();
    static HOUSE: OnceLock<Regex> = OnceLock::new();
    let india = INDIA.get_or_init(|| Regex::new(r"(?i)\b[1-9]\d{2}\s?\d{3}\b(?:\s*,?\s*india\b)?").unwrap());
    let house = HOUSE.get_or_init(|| Regex::new(r"(?i)^(?:#\s*|no\.?\s*|h\.?\s*no\.?\s*|flat\s+(?:no\.?\s*)?)?\d+[a-z]?(?:\s*[/\-]\s*\d+[a-z]?)?\b").unwrap());
    let us = US.get_or_init(|| {
        Regex::new(r"\b\d{1,6}\s+(?:[A-Z0-9][A-Za-z0-9.]*\s+){1,4}(?:Street|St|Avenue|Ave|Road|Rd|Lane|Ln|Boulevard|Blvd|Drive|Dr|Court|Ct|Way|Place|Pl|Terrace|Parkway|Pkwy|Circle|Cir|Highway|Hwy)\.?(?:,?\s*(?:Apt|Suite|Unit|#)\.?\s*[A-Za-z0-9\-]+)?,\s*[A-Z][A-Za-z .]{1,30},\s*[A-Z]{2}\s+\d{5}(?:-\d{4})?\b").unwrap()
    });
    let taken = |out: &Vec<PiiSpan>, a: usize, b: usize| out.iter().any(|s| s.start < b && a < s.end && s.kind != "phone");
    for m in us.find_iter(text) {
        if !taken(out, m.start(), m.end()) {
            let spans = address_spans(text, m.start(), m.end(), 0.85);
            out.retain(|s| !(s.kind == "phone" && s.start < m.end() && m.start() < s.end));
            out.extend(spans);
        }
    }
    for (start, field) in split_fields(text) {
        // The address ends at its PIN (and ", India"), after two commas.
        let Some(pin) = india.find_iter(field).find(|m| field[..m.start()].matches(',').count() >= 2) else { continue };
        let field = &field[..pin.end()];
        if field.len() > 160 || field[..pin.start()].chars().filter(|c| c.is_ascii_digit()).count() > 12 {
            continue;
        }
        let first = field.split(',').next().unwrap_or("");
        if !(house.is_match(first.trim()) || field.split(',').any(has_street_word)) {
            continue;
        }
        let end = start + field.len();
        if taken(out, start, end) {
            continue;
        }
        out.retain(|s| !(s.kind == "phone" && s.start < end && start < s.end));
        out.extend(address_spans(text, start, end, 0.8));
    }
}

/// A city with its PIN, on its own ("Bengaluru - 560038", "Pune, 411001"):
/// the PIN is `postal_code` and the city `location`. The six digits must
/// stand alone (not part of a longer number or a phone number).
fn city_pins(text: &str, out: &mut Vec<PiiSpan>) {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        Regex::new(r"\b((?:(?:New|Navi|Greater|North|South|East|West|Port|San|Los|Las|Santa|Fort)\s)?[A-Z][a-z]{2,})(\s*[-–,]\s*|\s)([1-9]\d{2}\s?\d{3})\b").unwrap()
    });
    for c in re.captures_iter(text) {
        let (city, sep, pin) = (c.get(1).unwrap(), c.get(2).unwrap(), c.get(3).unwrap());
        let after = &text[pin.end()..];
        if after.starts_with(|ch: char| ch.is_ascii_digit())
            || after.trim_start().starts_with(|ch: char| ch.is_ascii_digit())
            || after.starts_with(['-', '/', '.']) && after[1..].starts_with(|ch: char| ch.is_ascii_digit())
        {
            continue;
        }
        // With only a space between, the PIN must end the field ("Bengaluru
        // 560038"); "Sprint 202401 release" is not an address.
        if sep.as_str().trim().is_empty() && !after.trim_start().is_empty() && !after.trim_start().starts_with([',', '|', '.', ')', ';']) {
            continue;
        }
        if has_street_word(city.as_str()) || out.iter().any(|s| s.start < pin.end() && pin.start() < s.end && s.kind != "phone") {
            continue;
        }
        out.retain(|s| !(s.kind == "phone" && s.start < pin.end() && pin.start() < s.end));
        push(out, "postal_code", pin.start(), pin.end(), text, 0.85);
        if !out.iter().any(|s| s.start < city.end() && city.start() < s.end) {
            push(out, "location", city.start(), city.end(), text, 0.75);
        }
    }
}

/// "Mr. Anil Mehta", "Dr Priya Nair": a title and a capitalised name. Not
/// "Dr. Reddy's Laboratories", "Dr. B. R. Ambedkar University" or "MS Office".
fn titled_names(text: &str, out: &mut Vec<PiiSpan>) {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        Regex::new(r"\b(?:Mr|Mrs|Ms|Miss|Dr|Prof|Shri|Smt)\.?\s+(?:[A-Z]\.\s?){0,2}[A-Z][a-z]+(?:\s+[A-Z]\.)?(?:\s+[A-Z][a-z]+){0,2}\b").unwrap()
    });
    const NOT_AFTER: &[&str] = &[
        "university", "college", "institute", "hospital", "school", "laboratories", "labs", "memorial", "foundation",
        "trust", "academy", "clinic", "centre", "center", "road", "marg", "nagar", "street", "polytechnic",
    ];
    const NOT_NAME: &[&str] = &[
        "office", "excel", "word", "powerpoint", "access", "project", "teams", "outlook", "dynamics", "sql", "visio",
        "windows", "azure", "dos", "paint",
    ];
    for m in re.find_iter(text) {
        let words: Vec<String> = m.as_str().split_whitespace().skip(1).map(|w| w.trim_matches('.').to_lowercase()).collect();
        let next: Vec<String> = text[m.end()..].split_whitespace().take(4).map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).collect();
        // "B.Tech (Dr. Ambedkar ...": a college named after someone.
        if text[m.end()..].starts_with("'s")
            || text[..m.start()].trim_end().ends_with('(')
            || words.iter().any(|w| NOT_NAME.contains(&w.as_str()) || NOT_AFTER.contains(&w.as_str()))
            || next.iter().any(|w| NOT_AFTER.contains(&w.as_str()))
            || out.iter().any(|s| s.start < m.end() && m.start() < s.end && s.kind != "phone")
        {
            continue;
        }
        push(out, "person_name", m.start(), m.end(), text, 0.9);
    }
}

/// A name without its title ("Mr. Anil Mehta" -> "Anil Mehta").
pub(crate) fn strip_title(name: &str) -> &str {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"^(?:Mr|Mrs|Ms|Miss|Dr|Prof|Shri|Smt)\.?\s+").unwrap());
    re.find(name).map(|m| &name[m.end()..]).unwrap_or(name)
}

/// Split a line into "|"- or "•"-separated fields with their byte offsets.
fn split_fields(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if matches!(c, '|' | '•' | '\n') {
            out.push((start, &text[start..i]));
            start = i + c.len_utf8();
        }
    }
    out.push((start, &text[start..]));
    out.into_iter()
        .map(|(s, f)| {
            let trimmed = f.trim_start();
            (s + (f.len() - trimmed.len()), trimmed)
        })
        .filter(|(_, f)| !f.is_empty())
        .collect()
}

/// Replace every span with a placeholder such as "[EMAIL]".
pub fn redact(text: &str, spans: &[PiiSpan]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for s in spans {
        // A city, state or country is kept: it is needed, and does not
        // identify anyone on its own.
        if s.start < last || s.kind == "location" {
            continue;
        }
        out.push_str(&text[last..s.start]);
        out.push_str(&format!("[{}]", s.kind.to_uppercase()));
        last = s.end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(t: &str) -> Vec<&'static str> {
        detect(t).into_iter().map(|s| s.kind).collect()
    }

    #[test]
    fn finds_contact_details() {
        assert_eq!(kinds("Email: jane.doe@example.com"), vec!["email"]);
        assert!(kinds("+91 98765 43210").contains(&"phone"));
        assert!(!kinds("Invoice date 2026-11-13 due 13.12.2026").contains(&"phone"));
        let fr = detect("call +33 6 12 34 56 78 now");
        assert!(fr.iter().any(|s| s.kind == "phone" && s.text == "+33 6 12 34 56 78"));
        assert!(kinds("linkedin.com/in/jane-doe").contains(&"profile"));
        assert!(kinds("PAN: ABCDE1234F").contains(&"national_id"));
    }

    #[test]
    fn bare_domains_but_not_tech_terms() {
        assert!(kinds("portfolio-033.example.dev").contains(&"url"));
        assert!(kinds("ASP.NET, Node.js and Vue.js").is_empty());
        assert_eq!(kinds("jane@site.com"), vec!["email"]);
    }

    #[test]
    fn ignores_years_and_plain_text() {
        assert!(kinds("2019 - 2023").is_empty());
        assert!(kinds("Led a team of 14 across three time zones").is_empty());
    }

    #[test]
    fn profile_labels() {
        assert!(kinds("LinkedIn: jane.doe").contains(&"profile"));
        assert!(kinds("GitHub: janedoe | Location: Pune").contains(&"profile"));
    }

    #[test]
    fn labelled_fields() {
        assert!(kinds("Marital Status: Married").contains(&"marital_status"));
        assert!(kinds("Nationality: Indian | Gender: Female").contains(&"gender"));
    }

    #[test]
    fn names() {
        assert!(looks_like_name("Jane Doe"));
        assert!(looks_like_name("RAHUL KUMAR SHARMA"));
        assert!(!looks_like_name("Senior Software Engineer"));
        assert!(!looks_like_name("Professional Summary"));
    }

    #[test]
    fn redaction() {
        let t = "Mail: a@b.com, call +1 555 123 4567";
        let r = redact(t, &detect(t));
        assert!(!r.contains("a@b.com") && r.contains("[EMAIL]") && r.contains("[PHONE]"));
    }

    #[test]
    fn id_numbers_are_validated() {
        // Valid check digits are found; one changed digit is not.
        assert_eq!(kinds("Aadhaar 2345 6789 0124"), vec!["national_id"]);
        assert!(!kinds("Ref 2345 6789 0125").contains(&"national_id"));
        assert_eq!(kinds("PAN ABCPE1234F"), vec!["national_id"]);
        assert!(kinds("SSN 123-45-6789").contains(&"national_id"));
        assert!(!kinds("Part 666-12-3456").contains(&"national_id"));
        assert_eq!(kinds("DL MH12 20110012345"), vec!["driving_licence"]);
        assert_eq!(kinds("Card 4111 1111 1111 1111"), vec!["card_number"]);
        assert!(!kinds("Card 4111 1111 1111 1112").contains(&"card_number"));
        assert_eq!(kinds("IBAN GB82 WEST 1234 5698 7654 32"), vec!["bank_account"]);
        // Labelled values of any shape.
        assert_eq!(kinds("Driving Licence No.: D1234567"), vec!["driving_licence"]);
        assert_eq!(kinds("Social Security Number: 123456789"), vec!["national_id"]);
        assert_eq!(kinds("Passport: Z1234567"), vec!["national_id"]);
    }

    #[test]
    fn labels_vouch_for_the_value_only() {
        let t = "Email: jane.doe@example.com Pune, India";
        let e: Vec<PiiSpan> = detect(t).into_iter().filter(|s| s.kind == "email").collect();
        assert_eq!((e.len(), e[0].text.as_str(), e[0].confidence), (1, "jane.doe@example.com", 0.99));
        let p: Vec<PiiSpan> = detect("Phone:555-123-4567 linkedin.com/in/jane-doe-1234").into_iter().filter(|s| s.kind == "phone").collect();
        assert_eq!(p.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["555-123-4567"]);
        assert!(detect("E-Mail:: jane@example.com").iter().any(|s| s.kind == "email" && s.text == "jane@example.com"));
        // A label with no address or number after it is not one.
        assert!(!kinds("Contact: jane@example.com").contains(&"phone"));
        assert!(!kinds("Phone: available on request").contains(&"phone"));
    }

    #[test]
    fn spaced_addresses() {
        for t in ["jane.doe@example. com", "jane.doe@example .com", "Email: jane.doe @example.com"] {
            let e: Vec<PiiSpan> = detect(t).into_iter().filter(|s| s.kind == "email").collect();
            assert_eq!(e.len(), 1, "{t}");
            assert!(e[0].confidence >= 0.95, "{t}");
        }
        // Without a label a spaced "@" is only a guess.
        assert!(detect("jane.doe @example.com").iter().any(|s| s.kind == "email" && s.confidence < 0.8));
        // An address at the end of a sentence keeps its own domain.
        assert!(detect("Mail jane@example.com. In 2020 I moved").iter().any(|s| s.kind == "email" && s.text == "jane@example.com"));
    }

    #[test]
    fn phone_numbers_in_company() {
        let phones = |t: &str| detect(t).into_iter().filter(|s| s.kind == "phone").map(|s| s.text).collect::<Vec<_>>();
        // After a link that ends in digits.
        assert_eq!(phones("linkedin.com/in/jane-doe-12a45 9876543210 jane@example.com"), vec!["9876543210"]);
        assert_eq!(phones("www.linkedin.com/in/janedoe-abc-12345a67 555-123-4567"), vec!["555-123-4567"]);
        // Two numbers side by side.
        assert_eq!(phones("+91 9876543210 80412 34567"), vec!["+91 9876543210", "80412 34567"]);
        // The area code's opening bracket lost.
        assert_eq!(phones("jane@example.com 555) 123-4567"), vec!["555) 123-4567"]);
        assert_eq!(phones("Mobile: (555)123-4567"), vec!["(555)123-4567"]);
        assert!(phones("Since 2019) 2020-2023").is_empty());
        assert_eq!(phones("Contact No: +1 (555) 123 - 4567"), vec!["+1 (555) 123 - 4567"]);
        assert_eq!(phones("+91 – 9876543210 / 9876501234"), vec!["+91 – 9876543210", "9876501234"]);
        assert!(phones("2019 - 2023, 2020 – 2024").is_empty());
        // A label glued on by the text layer is not a link.
        assert_eq!(phones("Mob9876543210"), vec!["9876543210"]);
        assert_eq!(phones("Ph.9876543210"), vec!["9876543210"]);
    }

    #[test]
    fn years_are_not_numbers() {
        assert!(kinds("Awards 2008 2012 2015 2017 2019").is_empty());
        assert_eq!(kinds("Write to jane.doe@mail. com today"), vec!["email"]);
    }


    fn spans(t: &str) -> Vec<(&'static str, String)> {
        detect(t).into_iter().map(|s| (s.kind, s.text)).collect()
    }

    #[test]
    fn addresses_split_into_street_postal_code_and_place() {
        // Labelled: the value runs past its commas; the city stays.
        let a = spans("Address: 12, 3rd Cross, Indiranagar, Bengaluru 560038");
        assert!(a.contains(&("address", "12, 3rd Cross, Indiranagar".into())), "{a:?}");
        assert!(a.contains(&("postal_code", "560038".into())));
        assert!(a.contains(&("location", "Bengaluru".into())));
        assert_eq!(redact("Bengaluru", &detect("Bengaluru")), "Bengaluru");
        // The value stops at the next label.
        let b = spans("Residence: Flat 4B, Green Towers, Sector 21, Gurugram, Haryana 122001   Phone: +91 98765 43210");
        assert!(b.contains(&("address", "Flat 4B, Green Towers, Sector 21".into())), "{b:?}");
        assert!(b.contains(&("location", "Gurugram, Haryana".into())));
        assert!(b.iter().any(|(k, _)| *k == "phone"));
        // A place alone is a location, never removed.
        let t = "Location: Pune, Maharashtra";
        assert_eq!(spans(t), vec![("location", "Pune, Maharashtra".into())]);
        assert_eq!(redact(t, &detect(t)), t);
        // Unlabelled, in clear shapes only.
        let c = spans("12, 3rd Cross, Indiranagar, Bengaluru 560038");
        assert!(c.contains(&("address", "12, 3rd Cross, Indiranagar".into())), "{c:?}");
        let u = spans("1600 Elm Street, Springfield, IL 62704");
        assert!(u.contains(&("address", "1600 Elm Street".into())), "{u:?}");
        assert!(u.contains(&("postal_code", "62704".into())));
        let j = spans("Location: Austin, TX, USA Jan 2020 – Mar 2022");
        assert_eq!(j, vec![("location", "Austin, TX, USA".into())], "a job line's dates are not the place");
        let p = spans("Address: 12, 3rd Cross, Indiranagar, Pin: 560038");
        assert!(p.iter().all(|(k, t)| *k != "location" || !t.contains("Pin")), "{p:?}");
        for t in ["Infosys, Pune, Maharashtra", "Order 1200 Main Street parts, saved 12, 000 hours", "Raised output 2019 - 2023, 40% faster"] {
            assert!(spans(t).iter().all(|(k, _)| !matches!(*k, "address" | "postal_code")), "{t}: {:?}", spans(t));
        }
    }

    #[test]
    fn a_pin_next_to_its_city() {
        let a = spans("Bengaluru - 560038");
        assert!(a.contains(&("postal_code", "560038".into())) && a.contains(&("location", "Bengaluru".into())), "{a:?}");
        assert!(spans("New Delhi, 110001").contains(&("location", "New Delhi".into())));
        assert!(spans("Released Sprint 202401 to 3 regions").is_empty());
        // Part of a longer number is not a PIN.
        assert!(spans("Ticket Bengaluru 560038991").iter().all(|(k, _)| *k != "postal_code"));
    }

    #[test]
    fn upi_ids_and_profiles() {
        assert_eq!(spans("UPI: ravik88@okhdfc"), vec![("upi_id", "ravik88@okhdfc".into())]);
        assert!(spans("Pay ravik88@ybl").contains(&("upi_id", "ravik88@ybl".into())));
        assert_eq!(kinds("ravik88@paytm.com"), vec!["email"]);
        assert_eq!(spans("medium.com/@ravik"), vec![("profile", "medium.com/@ravik".into())]);
    }

    #[test]
    fn titled_names_and_referees() {
        assert_eq!(spans("Worked with Dr. Priya Nair on the migration."), vec![("person_name", "Dr. Priya Nair".into())]);
        assert!(spans("References: Mr. Anil Mehta, Manager, Infosys").contains(&("person_name", "Mr. Anil Mehta".into())));
        for t in [
            "References: available on request",
            "Worked at Dr. Reddy's Laboratories on MS Office and Ms Excel macros.",
            "Studied at Dr. B. R. Ambedkar University, Delhi",
            "B. Tech in Electronics (Dr. Babasaheb",
        ] {
            assert!(spans(t).is_empty(), "{t}: {:?}", spans(t));
        }
    }

}
