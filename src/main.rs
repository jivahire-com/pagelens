//! CLI.
//!   pagelens <file>                  print the analysis as JSON: PDF,
//!                                      DOCX, PPTX, HTML (images through
//!                                      OCR in builds with `ocr`); includes
//!                                      `person` (name, email, phone with
//!                                      confidence) and `needs_review`
//!                                      (scans, text images and doubtful
//!                                      details to read with an LLM)
//!   pagelens <a.pdf> <b.pdf> ...     one JSON object per line (batch:
//!                                      PDFium is loaded once for all files)
//!   pagelens --ner <file.pdf>        also tag names/places in running text
//!                                      (builds with `ner` only)
//!   pagelens --redact <file.pdf>     all raw personal data replaced by
//!                                      placeholders (for LLMs); NER on
//!                                      here when built in
//!   pagelens render <in.pdf> <out.pdf> [--logo logo.png] [--keep-pii] [--remove-pii]
//!                     [--note "text under the logo"] [--candidate-id "Candidate #1042"]
//!                                      re-typeset as a clean PDF, PII removed
//!   pagelens render <in> <out.pdf> --theme agency.json [--notes notes.json]
//!                     [--candidate-id "Candidate #1042"]
//!                                      the agency's branded format: every
//!                                      resume laid out the same way
//!   pagelens profile <file>          the resume as standard sections (JSON)
//! PDFium is built into the binary (and the OCR and name models in builds
//! with those features); set $PDFIUM_LIB_DIR to use an external PDFium.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "ner")]
    const PARSE: &str = "usage: pagelens [--ner] [--redact] <file.pdf|.docx|.pptx|.html|image> [more ...]";
    #[cfg(not(feature = "ner"))]
    const PARSE: &str = "usage: pagelens [--redact] <file.pdf|.docx|.pptx|.html|image> [more ...]";
    let usage = format!("{PARSE}\n{USAGE}");
    const USAGE: &str = "       pagelens render <in.pdf> <out.pdf> [--logo logo.png] [--keep-pii] [--remove-pii] [--note text] [--candidate-id label]\n       pagelens render <in> <out.pdf> --theme agency.json [--notes notes.json] [--candidate-id label] [--no-check]\n       any command: --known details.json (the candidate's name, email, phone; removed everywhere)\n       pagelens profile <file>\n       pagelens --version";
    match args.first().map(String::as_str) {
        None => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
        Some("--version" | "-V") => {
            println!("pagelens {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("{usage}\n\nOutput: one JSON object per input file, one per line.\nSettings: PAGELENS_OCR, PAGELENS_OCR_DPI, PAGELENS_OCR_IMAGES, PAGELENS_NER, PAGELENS_VISIBILITY, PDFIUM_LIB_DIR (see README).");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    // --known details.json: details of the person that must be removed
    // wherever they appear ({"name", "email", "phone", "other": [...]}).
    let mut args = args;
    if let Some(i) = args.iter().position(|a| a == "--known") {
        let Some(path) = args.get(i + 1).cloned() else {
            eprintln!("--known needs a JSON file");
            return ExitCode::from(2);
        };
        let known = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<pagelens::pii::Known>(&t).map_err(|e| e.to_string()));
        match known {
            Ok(k) => pagelens::pii::set_known(Some(k)),
            Err(e) => {
                eprintln!("known details {path}: {e}");
                return ExitCode::from(2);
            }
        }
        args.drain(i..i + 2);
    }
    // PDFium: $PDFIUM_LIB_DIR if set, else the copy embedded in this binary.
    let lib_dir = std::env::var_os("PDFIUM_LIB_DIR").map(PathBuf::from);
    let pdfium = match pagelens::backend::bind_default(lib_dir.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if args[0] == "render" {
        #[cfg(feature = "ner")]
        pagelens::ner::enable(); // catch every name before re-publishing
        pagelens::backend::enable_visibility_check(); // and all hidden text
        return render(&pdfium, &args[1..]);
    }
    if args[0] == "profile" {
        #[cfg(feature = "ner")]
        pagelens::ner::enable();
        let mut status = ExitCode::SUCCESS;
        for path in &args[1..] {
            match analyse_file(&pdfium, path) {
                Ok(doc) => {
                    let p = pagelens::profile::build(&doc, "[PERSON_NAME]", false);
                    println!("{}", serde_json::to_string(&p).unwrap());
                }
                Err(e) => {
                    eprintln!("cannot read {path}: {e}");
                    status = ExitCode::from(1);
                }
            }
        }
        return status;
    }
    // --redact: output with all raw personal data removed (for AI workloads).
    let redact = args.iter().any(|a| a == "--redact");
    // --ner: also find names and places in running text (slower, about
    // 0.1-1 s per page); always on with --redact.
    #[cfg(feature = "ner")]
    if redact || args.iter().any(|a| a == "--ner") {
        pagelens::ner::enable();
    }
    // Options this build does not know (including --ner without the name
    // model) are an error, never silently ignored or read as a file name.
    let known_option = |a: &str| a == "--redact" || (cfg!(feature = "ner") && a == "--ner");
    if let Some(bad) = args.iter().find(|a| a.starts_with("--") && !known_option(a)) {
        let why = if bad == "--ner" { " (this build has no name model; names come from the rules)" } else { "" };
        eprintln!("unknown option {bad}{why}\n{usage}");
        return ExitCode::from(2);
    }
    if redact {
        pagelens::backend::enable_visibility_check();
    }
    let args: Vec<String> = args.into_iter().filter(|a| !known_option(a)).collect();
    if args.is_empty() {
        eprintln!("{usage}");
        return ExitCode::from(2);
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut failed = false;
    for pdf in &args {
        match analyse_file(&pdfium, pdf) {
            Ok(result) => {
                let result = if redact { pagelens::redact_doc(result) } else { result };
                serde_json::to_writer(&mut out, &result).expect("serialisable");
                writeln!(out).ok();
            }
            Err(e) => {
                eprintln!("cannot read {pdf}: {e}");
                failed = true;
                if args.len() > 1 {
                    writeln!(out, "{{\"error\":{}}}", serde_json::to_string(&e.to_string()).unwrap()).ok();
                }
            }
        }
    }
    out.flush().ok();
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

/// Read the written PDF back and make sure no personal data from the input
/// is left in it. On a leak the file is deleted, so it cannot be shared.
fn output_is_clean(pdfium: &pdfium_render::prelude::Pdfium, doc: &pagelens::DocResult, output: &str, allowed: &[String]) -> bool {
    let text = match pagelens::backend::load_pages(pdfium, Path::new(output)) {
        Ok(pages) => pagelens::analyse(&pages).text,
        Err(e) => {
            eprintln!("cannot check {output}: {e}");
            std::fs::remove_file(output).ok();
            return false;
        }
    };
    let leaks = pagelens::check::leaks(doc, &text, allowed, pagelens::pii::known().as_ref());
    if leaks.is_empty() {
        return true;
    }
    std::fs::remove_file(output).ok();
    let kinds: Vec<&str> = leaks.iter().map(|l| l.kind).collect();
    eprintln!("refused: {output} still contained personal data ({}); the file was deleted. Review the input, or pass --known with the candidate's details.", kinds.join(", "));
    false
}

/// Analyse any supported input: PDF, image (OCR), DOCX, PPTX or HTML.
fn analyse_file(pdfium: &pdfium_render::prelude::Pdfium, path: &str) -> Result<pagelens::DocResult, String> {
    let p: &Path = path.as_ref();
    if pagelens::structured::is_structured_file(p) {
        return pagelens::structured::load(p).map(pagelens::structured::analyse_blocks);
    }
    let pages = if pagelens::backend::is_image_file(p) {
        load_image(path)?
    } else {
        pagelens::backend::load_pages(pdfium, p).map_err(|e| e.to_string())?
    };
    Ok(pagelens::analyse(&pages))
}

#[cfg(feature = "ocr")]
fn load_image(path: &str) -> Result<Vec<pagelens::model::PageContent>, String> {
    pagelens::backend::load_image(path.as_ref())
}

/// Without OCR an image file is one unread page: the result lists it in
/// `needs_review` for the caller to read (for example with a vision model).
#[cfg(not(feature = "ocr"))]
fn load_image(path: &str) -> Result<Vec<pagelens::model::PageContent>, String> {
    if !Path::new(path).is_file() {
        return Err(format!("{path}: no such file"));
    }
    Ok(vec![pagelens::model::PageContent {
        ocr: Some(pagelens::ocr::OcrStatus { reason: "image file".into(), engine: None, words: 0 }),
        ..Default::default()
    }])
}

fn render(pdfium: &pdfium_render::prelude::Pdfium, args: &[String]) -> ExitCode {
    let mut positional = Vec::new();
    let mut logo: Option<PathBuf> = None;
    let mut note: Option<String> = None;
    let mut candidate: Option<String> = None;
    let mut redact = true;
    let mut remove = false;
    let mut theme: Option<PathBuf> = None;
    let mut notes: Option<PathBuf> = None;
    let mut check = true;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--logo" => logo = it.next().map(PathBuf::from),
            "--note" => note = it.next().cloned(),
            "--candidate-id" => candidate = it.next().cloned(),
            "--keep-pii" => redact = false,
            "--remove-pii" => remove = true,
            "--theme" => theme = it.next().map(PathBuf::from),
            "--notes" => notes = it.next().map(PathBuf::from),
            "--no-check" => check = false,
            _ => positional.push(a.clone()),
        }
    }
    let [input, output] = positional.as_slice() else {
        eprintln!("usage: pagelens render <in.pdf> <out.pdf> [--logo logo.png] [--keep-pii] [--remove-pii] [--note text] [--candidate-id label]");
        return ExitCode::from(2);
    };
    let doc = match analyse_file(pdfium, input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {input}: {e}");
            return ExitCode::from(1);
        }
    };
    if let Some(theme) = theme {
        let theme = match pagelens::brand::Theme::load(&theme) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        };
        let notes = match notes.as_deref().map(pagelens::brand::load_notes).transpose() {
            Ok(n) => n.unwrap_or_default(),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        };
        let label = candidate.unwrap_or_else(|| "Candidate".to_string());
        let mut profile = pagelens::profile::build(&doc, &label, theme.show_location);
        if theme.hide_current_employer {
            profile.hide_current_employer("Confidential");
        }
        if profile.looks_empty() {
            eprintln!("warning: {input}: no experience, education or skills found; this may not be a resume");
        }
        let mut allowed = vec![theme.agency.clone(), theme.footer.clone().unwrap_or_default(), label.clone(), profile.location.clone()];
        allowed.extend(notes.iter().flat_map(|(t, x)| [t.clone(), x.clone()]));
        let branding = pagelens::brand::Branding { theme: &theme, label, notes };
        return match pagelens::brand::write_branded(pdfium, &profile, Path::new(output), &branding) {
            Ok(pages) => {
                if check && !output_is_clean(pdfium, &doc, output, &allowed) {
                    return ExitCode::from(3);
                }
                eprintln!("wrote {output} ({pages} pages, {} PII items removed, output checked)", doc.pii.len());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("cannot write {output}: {e}");
                ExitCode::from(1)
            }
        };
    }
    let allowed: Vec<String> = [candidate.clone(), note.clone()].into_iter().flatten().collect();
    let opts =
        pagelens::render::RenderOptions { logo: logo.as_deref(), redact, candidate_label: candidate, header_note: note, remove };
    match pagelens::render::write_pdf(pdfium, &doc, Path::new(output), &opts) {
        Ok(()) => {
            if redact && check && !output_is_clean(pdfium, &doc, output, &allowed) {
                return ExitCode::from(3);
            }
            eprintln!("wrote {output} ({} PII items {})", doc.pii.len(), if redact { "removed" } else { "kept" });
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot write {output}: {e}");
            ExitCode::from(1)
        }
    }
}
