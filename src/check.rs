//! Final check before a file is shared: read the written output back and
//! make sure none of the personal data found in the input is still there.
//!
//! Three things count as a leak:
//! 1. anything the pattern detectors find in the output (an email, phone
//!    number, ID number, link) that the agency did not add itself;
//! 2. any value removed from the input (the name, email, phone, IDs), and any
//!    part of the person's name, written as a name;
//! 3. any detail the caller said it knows (`pii::Known`).

use crate::{pii, DocResult};

/// One piece of personal data still present in the output: its type only,
/// so the report itself never repeats the data.
#[derive(Debug, Clone, PartialEq)]
pub struct Leak {
    pub kind: &'static str,
}

/// Compare the analysed input with the text read back from the output.
/// `allowed` is text the output may contain on purpose: the agency's name
/// and footer, the candidate label, recruiter notes, a shown location.
pub fn leaks(source: &DocResult, output_text: &str, allowed: &[String], known: Option<&pii::Known>) -> Vec<Leak> {
    let allowed: Vec<String> = allowed.iter().map(|a| a.to_ascii_lowercase()).collect();
    let is_allowed = |v: &str| {
        let v = v.trim().to_ascii_lowercase();
        allowed.iter().any(|a| a.contains(&v))
    };
    let mut out: Vec<Leak> = Vec::new();
    let mut push = |kind: &'static str| {
        if !out.iter().any(|l| l.kind == kind) {
            out.push(Leak { kind });
        }
    };

    // 1. Pattern matches in the output.
    for line in output_text.lines() {
        for s in pii::detect(line) {
            if !is_allowed(&s.text) {
                push(s.kind);
            }
        }
    }

    // 2. Values removed from the input.
    let lower = output_text.to_ascii_lowercase();
    for f in &source.pii {
        let v = f.text.trim();
        if v.chars().count() >= 4 && !is_allowed(v) && contains_word(output_text, &lower, v, false) {
            push(f.kind);
        }
    }
    // Parts of the person's name (the names on the first page), written
    // as a name: capitalised, as in the input.
    let mut names: Vec<String> =
        source.pii.iter().filter(|f| f.kind == "person_name" && f.page == 1 && f.block < 8).map(|f| f.text.clone()).collect();
    if let Some(k) = known {
        names.extend(k.name.iter().cloned());
    }
    for n in &names {
        for part in n.split(|c: char| !c.is_alphabetic()) {
            if part.chars().count() >= 3
                && !pii::NOT_NAME_WORDS.contains(&part.to_lowercase().as_str())
                && !is_allowed(part)
                && contains_word(output_text, &lower, part, true)
            {
                push("person_name");
            }
        }
    }

    // 3. Known details in any spacing.
    if let Some(k) = known {
        if let Some(e) = &k.email {
            if lower.contains(&e.trim().to_ascii_lowercase()) {
                push("email");
            }
        }
        if let Some(p) = &k.phone {
            let want: String = p.chars().filter(|c| c.is_ascii_digit()).collect();
            let tail = &want[want.len().saturating_sub(10)..];
            let have: String = output_text.chars().filter(|c| c.is_ascii_digit()).collect();
            if tail.len() >= 7 && have.contains(tail) {
                push("phone");
            }
        }
        for o in &k.other {
            if o.trim().chars().count() >= 3 && lower.contains(&o.trim().to_ascii_lowercase()) {
                push("personal_data");
            }
        }
    }
    out
}

/// Whether `text` contains `word` as a whole word, ignoring case. With
/// `capital`, only occurrences that start with a capital letter count.
fn contains_word(text: &str, lower: &str, word: &str, capital: bool) -> bool {
    let needle = word.to_ascii_lowercase();
    lower.match_indices(&needle).any(|(pos, _)| {
        let end = pos + needle.len();
        let edge = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
        !edge(text[..pos].chars().next_back())
            && !edge(text[end..].chars().next())
            && (!capital || text[pos..].chars().next().is_some_and(|c| c.is_uppercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(findings: &[(&'static str, &str)]) -> DocResult {
        let mut d = crate::analyse(&[]);
        for (kind, text) in findings {
            d.pii.push(crate::PiiFinding { kind, text: text.to_string(), page: 1, block: 0, confidence: 0.9 });
        }
        d
    }

    #[test]
    fn finds_what_is_left() {
        let d = doc_with(&[("person_name", "Asha Example"), ("email", "asha@example.com")]);
        assert!(leaks(&d, "Candidate #7\nSenior engineer", &["Candidate #7".into()], None).is_empty());
        let l = leaks(&d, "Candidate #7\nASHA led the team", &[], None);
        assert_eq!(l, vec![Leak { kind: "person_name" }]);
        let l = leaks(&d, "write to asha@example.com", &[], None);
        assert!(l.iter().any(|x| x.kind == "email"));
        // The agency's own contact details are allowed.
        assert!(leaks(&d, "jobs@agency.example", &["Contact jobs@agency.example".into()], None).is_empty());
        let k = pii::Known { phone: Some("+91 98765 43210".into()), ..Default::default() };
        assert_eq!(leaks(&d, "ref 98765-43210", &[], Some(&k)).len(), 1);
    }
}
