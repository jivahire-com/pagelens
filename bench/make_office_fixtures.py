"""Generate the synthetic DOCX and PPTX test fixtures in tests/fixtures.

    python bench/make_office_fixtures.py      (needs python-docx, python-pptx)

All content is invented; the email and phone are reserved example values.
"""
from pathlib import Path

from docx import Document
from pptx import Presentation
from pptx.util import Inches

OUT = Path(__file__).resolve().parent.parent / "tests" / "fixtures"


def make_docx(path: Path) -> None:
    d = Document()
    d.add_heading("Asha Example", level=0)  # Title style
    d.add_paragraph("Email: asha@example.com | Phone: +91 98765 43210")
    d.add_heading("Experience", level=1)
    d.add_paragraph("Built data pipelines in Rust and Python for invoice processing.")
    d.add_paragraph("Led a team of four engineers", style="List Bullet")
    d.add_paragraph("Cut parsing cost by 60 percent", style="List Bullet")
    d.add_heading("Skills", level=2)
    t = d.add_table(rows=3, cols=3)
    for r, row in enumerate([["Skill", "Years", "Level"], ["Rust", "4", "Expert"], ["SQL", "6", "Advanced"]]):
        for c, val in enumerate(row):
            t.cell(r, c).text = val
    d.add_paragraph("References available on request.")
    d.save(path)


def make_pptx(path: Path) -> None:
    p = Presentation()
    s = p.slides.add_slide(p.slide_layouts[0])  # title slide
    s.shapes.title.text = "Quarterly Review"
    s.placeholders[1].text = "Operations team"
    s = p.slides.add_slide(p.slide_layouts[1])  # title and content
    s.shapes.title.text = "Highlights"
    body = s.placeholders[1].text_frame
    body.text = "Revenue up 12 percent"
    body.add_paragraph().text = "Two new regions launched"
    s = p.slides.add_slide(p.slide_layouts[5])  # title only
    s.shapes.title.text = "Numbers"
    rows = [["Region", "Q1", "Q2"], ["North", "120", "140"], ["South", "90", "115"]]
    table = s.shapes.add_table(3, 3, Inches(1), Inches(2), Inches(6), Inches(1.5)).table
    for r, row in enumerate(rows):
        for c, val in enumerate(row):
            table.cell(r, c).text = val
    p.save(path)


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    make_docx(OUT / "resume.docx")
    make_pptx(OUT / "deck.pptx")
    (OUT / "page.html").write_text(
        "<!doctype html><html><head><title>Example</title><style>h1{}</style></head><body>"
        "<h1>Asha Example</h1><p>Email: asha@example.com</p>"
        "<h2>Skills</h2><ul><li>Rust</li><li>Python &amp; SQL</li></ul>"
        "<table><tr><th>Year</th><th>Role</th></tr><tr><td>2024</td><td>Engineer</td></tr></table>"
        "<div hidden>keyword stuffing</div><script>var x=1;</script></body></html>\n"
    )
    print("wrote", OUT)
