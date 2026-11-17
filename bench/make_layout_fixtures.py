"""Synthetic fixtures for page-level layout problems found on real resumes (all content invented).

    .venv/bin/python bench/make_layout_fixtures.py

Writes tests/fixtures/multipage_resume.pdf: two pages with
  * a running header (name and email) and a "Page N of 2" footer on both pages,
  * section headings set in plain body type ("Experience", "Education", "Skills"),
  * bullets drawn as small vector shapes, not as characters,
  * a numbered list,
  * a table that starts on page 1 and continues on page 2 with its header row repeated,
  * a list whose bullets are all drawn by one path object,
  * a section heading set tight under the last bullet above it (no full stop).
Also spaced_bullets.pdf, resume_image_badges.pdf and resume_links.pdf (see each function).
"""
from pathlib import Path

from reportlab.lib.colors import black
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen.canvas import Canvas
from reportlab.pdfbase.pdfmetrics import stringWidth

OUT = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "multipage_resume.pdf"
W, H = A4
X, BODY = 60, 10.5


def running(c, page):
    c.setFont("Helvetica", 8)
    c.drawString(X, H - 30, "Alex Example | alex.example@example.com")
    c.drawRightString(W - X, 30, f"Page {page} of 2")


def bullets(c, y, items, shape):
    c.setFont("Helvetica", BODY)
    for text in items:
        c.setFillColor(black)
        if shape == "dot":
            c.circle(X + 3, y + 3.5, 2.0, stroke=0, fill=1)
        else:
            c.rect(X + 1, y + 1.5, 4, 4, stroke=0, fill=1)
        c.drawString(X + 14, y, text)
        y -= 15
    return y


def table(c, y, rows):
    col = [X, X + 160, X + 330]
    c.setFont("Helvetica-Bold", BODY)
    for i, h in enumerate(("Course", "Institute", "Year")):
        c.drawString(col[i], y, h)
    c.line(X, y - 4, W - X, y - 4)
    y -= 18
    c.setFont("Helvetica", BODY)
    for r in rows:
        for i, v in enumerate(r):
            c.drawString(col[i], y, v)
        c.line(X, y - 4, W - X, y - 4)
        y -= 18
    return y


def main():
    c = Canvas(str(OUT), pagesize=A4)
    # Page 1
    running(c, 1)
    c.setFont("Helvetica-Bold", 20)
    c.drawString(X, H - 80, "Alex Example")
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 98, "Data engineer · Springfield")
    y = H - 140
    c.drawString(X, y, "Experience")  # a section heading in plain body type
    y -= 20
    c.drawString(X, y, "Senior data engineer, Northwind Traders, 2019 to now")
    y -= 18
    y = bullets(c, y, ["Built a streaming pipeline for order events",
                       "Cut nightly batch time from six hours to two",
                       "Led a team of four engineers"], "dot")
    y -= 14
    c.drawString(X, y, "Data engineer, Contoso Analytics, 2015 to 2019")
    y -= 18
    y = bullets(c, y, ["Designed the warehouse schema for sales reporting",
                       "Moved reporting jobs to a shared scheduler"], "square")
    y -= 14
    c.drawString(X, y, "Key projects")
    y -= 18
    for i, text in enumerate(["Customer churn model for the retail team",
                              "Data quality checks across all ingest jobs"], 1):
        c.drawString(X, y, f"{i}. {text}")
        y -= 15
    y -= 14
    c.drawString(X, y, "Education")
    y -= 22
    table(c, y, [("BSc Computer Science", "Springfield University", "2015"),
                 ("Diploma in Statistics", "Shelbyville College", "2012")])
    c.showPage()
    # Page 2: the table continues with its header row repeated
    running(c, 2)
    y = H - 80
    y = table(c, y, [("Cloud Data Certificate", "Online Academy", "2020")])
    y -= 24
    c.setFont("Helvetica", BODY)
    c.drawString(X, y, "Skills")
    y -= 20
    c.drawString(X, y, "Python, SQL, Spark, Airflow, Kafka")
    y -= 26
    c.drawString(X, y, "Languages")
    y -= 18
    # Every bullet of this list in a single path object.
    p = c.beginPath()
    langs = ["English, fluent", "Spanish, working knowledge"]
    for k in range(len(langs)):
        p.circle(X + 3, y - 15 * k + 3.5, 2.0)
    c.drawPath(p, stroke=0, fill=1)
    for k, text in enumerate(langs):
        c.drawString(X + 14, y - 15 * k, text)
    y -= 15 * len(langs) - 4
    # A section heading straight under the last bullet, which has no full stop.
    c.drawString(X, y, "Project Description")
    y -= 15
    c.drawString(X, y, "A reporting portal for regional sales teams.")
    c.showPage()
    c.save()
    print(f"wrote {OUT}")


SPACED = OUT.with_name("spaced_bullets.pdf")


def spaced():
    """One page at 1.5 line spacing: wrapped bullets (character and drawn),
    then two paragraphs that must stay apart."""
    c = Canvas(str(SPACED), pagesize=A4)
    lead = BODY * 1.8  # 1.5 lines: baseline to baseline
    y = H - 80
    c.setFont("Helvetica", BODY)
    c.drawString(X, y, "Responsibilities")
    y -= 2 * lead
    items = [
        ["Owned the order events pipeline end to end, from the Kafka topics",
         "through the warehouse loaders to the finance dashboards"],
        ["Reviewed every schema change with the analytics team before it",
         "reached production, and kept the data dictionary current"],
    ]
    for drawn in (False, True):
        for lines in items:
            if drawn:
                c.circle(X + 3, y + 3.5, 2.0, stroke=0, fill=1)
            else:
                c.drawString(X, y, "\u2022")
            for k, text in enumerate(lines):
                c.drawString(X + 14, y - k * lead, text)
            y -= len(lines) * lead
        y -= lead
    c.drawString(X, y, "The team moved to a new scheduler in the spring.")
    y -= 2 * lead
    c.drawString(X, y, "A second paragraph starts after a blank line.")
    c.showPage()
    c.save()
    print(f"wrote {SPACED}")


BADGES = OUT.with_name("resume_image_badges.pdf")


def badges():
    """A resume whose skills are short labels set over a picture (a design
    panel exported as an image): the labels are content, not chart labels."""
    import pymupdf

    png = BADGES.with_suffix(".png")
    pix = pymupdf.Pixmap(pymupdf.csRGB, pymupdf.IRect(0, 0, 400, 160), False)
    pix.set_rect(pix.irect, (225, 235, 250))
    pix.save(str(png))
    c = Canvas(str(BADGES), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 98, "sam.sample@example.com")
    c.drawString(X, H - 140, "Skills")
    c.drawImage(str(png), X, H - 260, width=240, height=96)
    for k, label in enumerate(["Python", "Airflow", "Terraform", "Kubernetes"]):
        c.drawString(X + 12 + 115 * (k % 2), H - 195 - 30 * (k // 2), label)
    c.drawString(X, H - 300, "Experience")
    c.drawString(X, H - 320, "Platform engineer, Fabrikam, 2018 to now")
    c.showPage()
    c.save()
    png.unlink()
    print(f"wrote {BADGES}")


LINKS = OUT.with_name("resume_links.pdf")


def links():
    """Contact links: a label that hides its address, one that shows it, a
    label wrapped over two lines, and an icon with no text whose address is
    nowhere else on the page."""
    c = Canvas(str(LINKS), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.setFont("Helvetica", BODY)
    y = H - 100
    c.drawString(X, y, "LinkedIn")
    c.linkURL("https://www.linkedin.com/in/sam-sample-0000", (X, y - 2, X + 45, y + 10), relative=0)
    c.drawString(X + 70, y, "sam.sample@example.com")
    c.linkURL("mailto:sam.sample@example.com", (X + 70, y - 2, X + 200, y + 10), relative=0)
    c.rect(X + 220, y - 1, 9, 9, stroke=0, fill=1)  # a GitHub icon, no text
    c.linkURL("https://github.com/sample-dev-0000", (X + 219, y - 2, X + 230, y + 9), relative=0)
    c.drawString(X, H - 140, "Projects")
    c.drawString(X, H - 158, "Order tracking app, see the live")
    c.drawString(X, H - 172, "demo site for details")
    c.linkURL("https://orders.example.org/demo", (X, H - 175, X + 165, H - 156), relative=0)
    c.showPage()
    c.save()
    print(f"wrote {LINKS}")


FORM_BULLETS = OUT.with_name("form_bullets.pdf")


def form_bullets():
    """A list whose bullets (drawn dots) and text sit inside a form XObject
    placed with a translation, as some Word and design-tool exports do."""
    c = Canvas(str(FORM_BULLETS), pagesize=A4)
    c.setFont("Helvetica-Bold", 16)
    c.drawString(X, H - 80, "Responsibilities")
    c.beginForm("list", lowerx=0, lowery=-60, upperx=400, uppery=20)
    c.setFont("Helvetica", BODY)
    for k, text in enumerate(["Ran the release train for four teams",
                              "Wrote the incident playbooks",
                              "Moved builds to a shared cache"]):
        c.circle(3, -15 * k + 3.5, 2.0, stroke=0, fill=1)
        c.drawString(14, -15 * k, text)
    c.endForm()
    c.saveState()
    c.translate(X, H - 110)
    c.doForm("list")
    c.restoreState()
    c.showPage()
    c.save()
    print(f"wrote {FORM_BULLETS}")


SECTION_TABLE = OUT.with_name("section_in_table.pdf")


def section_in_table():
    """A resume laid out as one ruled table, with a section name alone in a
    full-width row between rows of details."""
    c = Canvas(str(SECTION_TABLE), pagesize=A4)
    c.setFont("Helvetica", BODY)
    col = [X, X + 160, X + 330, W - X]
    rows = [("Degree", "Institute", "Year"), ("BSc Physics", "Riverside College", "2016"),
            ("Experience", None, None), ("Analyst", "Fabrikam", "2017 to 2020"),
            ("Senior analyst", "Contoso", "2020 to now")]
    y = H - 100
    c.line(X, y + 14, W - X, y + 14)
    for r in rows:
        if r[1] is None:  # one merged cell across the row
            c.drawString(X + 4, y, r[0])
            for x in (col[0], col[3]):
                c.line(x, y + 14, x, y - 4)
        else:
            for i, v in enumerate(r):
                c.drawString(col[i] + 4, y, v)
            for x in col:
                c.line(x, y + 14, x, y - 4)
        c.line(X, y - 4, W - X, y - 4)
        y -= 18
    c.showPage()
    c.save()
    print(f"wrote {SECTION_TABLE}")


def names():
    """Name at the top of a resume, with different evidence: matching the
    email, with no email, and with an email that does not match."""
    cases = {
        "name_email_match.pdf": ("Priya Raman", "priya.raman.dev@example.com"),
        "name_no_email.pdf": ("Arjun Mehta", None),
        "name_email_differs.pdf": ("Kavya Iyer", "hireme2024@example.com"),
    }
    for fname, (name, email) in cases.items():
        c = Canvas(str(OUT.with_name(fname)), pagesize=A4)
        c.setFont("Helvetica-Bold", 20)
        c.drawString(X, H - 80, name)
        c.setFont("Helvetica", BODY)
        c.drawString(X, H - 100, " | ".join(x for x in (email, "+91 98765 43210", "Pune") if x))
        c.drawString(X, H - 140, "Experience")
        c.drawString(X, H - 158, "Data analyst, Northwind Traders, 2019 to now")
        c.drawString(X, H - 190, "Education")
        c.drawString(X, H - 208, "BSc Statistics, Riverside College, 2018")
        c.showPage()
        c.save()
        print(f"wrote {fname}")
    # A line of small print above the name, set tight so both join into one
    # block, all in body type and with no email address.
    c = Canvas(str(OUT.with_name("name_second_line.pdf")), pagesize=A4)
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 80, "this profile was shared with consent")
    c.drawString(X, H - 93, "Rohan Deshpande")
    c.drawString(X, H - 106, "+91 98765 43210")
    c.drawString(X, H - 140, "Experience")
    c.drawString(X, H - 158, "Support engineer, Fabrikam, 2020 to now")
    c.drawString(X, H - 190, "Education")
    c.drawString(X, H - 208, "BCom, Riverside College, 2019")
    c.showPage()
    c.save()
    print("wrote name_second_line.pdf")
    # A name-shaped line (an office address) above the real name, which
    # matches the email address.
    c = Canvas(str(OUT.with_name("name_email_beats_shape.pdf")), pagesize=A4)
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 80, "Riverside Business Park")
    c.setFont("Helvetica-Bold", 14)
    c.drawString(X, H - 100, "Neha Kulkarni")
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 118, "neha.kulkarni@example.com | +91 98765 43210")
    c.drawString(X, H - 150, "Experience")
    c.drawString(X, H - 168, "Analyst, Contoso, 2021 to now")
    c.showPage()
    c.save()
    print("wrote name_email_beats_shape.pdf")


def unread_images():
    """Without OCR: a scanned page (image only), and a digital resume with a
    picture of printed text and a photo-like picture (must not be listed)."""
    import io
    import numpy as np
    from PIL import Image, ImageDraw, ImageFont

    font = ImageFont.load_default(size=28)
    text_img = Image.new("RGB", (900, 220), "white")
    d = ImageDraw.Draw(text_img)
    for k, line in enumerate(["AWS Certified Solutions Architect", "Associate, 2023", "Credential 0000-SAMPLE"]):
        d.text((20, 20 + 64 * k), line, fill="black", font=font)
    rng = np.random.default_rng(5)
    yy, xx = np.mgrid[0:240, 0:200]
    photo = np.stack([(xx * 1.1) % 255, (yy * 0.9 + 40) % 255, ((xx + yy) * 0.6) % 255], -1)
    photo = Image.fromarray((photo + rng.normal(0, 18, photo.shape)).clip(0, 255).astype(np.uint8))

    def png(img):
        b = io.BytesIO()
        img.save(b, "PNG")
        b.seek(0)
        return b

    def jpeg(img):
        b = io.BytesIO()
        img.save(b, "JPEG", quality=60)
        b.seek(0)
        return b

    from reportlab.lib.utils import ImageReader
    c = Canvas(str(OUT.with_name("resume_text_image.pdf")), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 100, "sam.sample@example.com")
    c.drawImage(ImageReader(jpeg(photo)), W - X - 80, H - 130, width=80, height=96)
    c.drawString(X, H - 160, "Certifications")
    c.drawImage(ImageReader(png(text_img)), X, H - 260, width=300, height=73)
    c.drawString(X, H - 300, "Experience")
    c.drawString(X, H - 318, "Platform engineer, Fabrikam, 2018 to now")
    c.showPage()
    c.save()
    print("wrote resume_text_image.pdf")

    page = Image.new("RGB", (1240, 1754), "white")
    d = ImageDraw.Draw(page)
    for k, line in enumerate(["Sam Sample", "sam.sample@example.com", "Experience", "Platform engineer, Fabrikam"]):
        d.text((120, 150 + 70 * k), line, fill="black", font=font)
    c = Canvas(str(OUT.with_name("scanned_page.pdf")), pagesize=A4)
    c.drawImage(ImageReader(png(page)), 0, 0, width=W, height=H)
    c.showPage()
    c.save()
    print("wrote scanned_page.pdf")

    # A contact line exported as a picture, one text line high, next to a
    # rating bar and a divider of the same shape (must not be listed).
    small = ImageFont.load_default(size=22)
    strip = Image.new("RGB", (330, 30), "white")
    ImageDraw.Draw(strip).text((2, 3), "+1 555 123 4567", fill="black", font=small)
    bar = Image.new("RGB", (330, 30), "white")
    ImageDraw.Draw(bar).rectangle((0, 6, 110, 24), fill="black")
    rule = Image.new("RGB", (330, 30), "white")
    ImageDraw.Draw(rule).rectangle((0, 13, 329, 16), fill="black")
    c = Canvas(str(OUT.with_name("resume_contact_strip.pdf")), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.drawImage(ImageReader(png(strip)), X, H - 104, width=110, height=10)
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 140, "Skills")
    c.drawString(X, H - 158, "Python")
    c.drawImage(ImageReader(png(bar)), X + 120, H - 160, width=110, height=10)
    c.drawImage(ImageReader(png(rule)), X, H - 180, width=110, height=10)
    c.drawString(X, H - 210, "Experience")
    c.drawString(X, H - 228, "Platform engineer, Fabrikam, 2018 to now")
    c.showPage()
    c.save()
    print("wrote resume_contact_strip.pdf")


def contacts():
    """Email and phone written the ways real text layers give them: a label
    with more words after the value, a space inside the address, a number
    straight after a link that ends in digits, a number wrapped onto the next
    line, and a mailto link over only a piece of the address."""
    def page(name, lines, links=()):
        path = OUT.with_name(f"contact_{name}.pdf")
        c = Canvas(str(path), pagesize=A4)
        c.setFont("Helvetica-Bold", 18)
        c.drawString(X, H - 80, "Sam Sample")
        c.setFont("Helvetica", BODY)
        for y, text in lines:
            c.drawString(X, H - y, text)
        for uri, (x0, y, x1) in links:
            c.linkURL(uri, (X + x0, H - y - 2, X + x1, H - y + 10), relative=0)
        c.drawString(X, H - 260, "Experience")
        c.drawString(X, H - 278, "Platform engineer at Fabrikam, 2019 - 2024")
        c.showPage()
        c.save()
        print(f"wrote {path}")

    page("label_words", [(104, "Email: sam.sample@example.com Pune, India"),
                         (118, "Phone:555-123-4567 linkedin.com/in/sam-sample-1234")])
    page("spaced", [(104, "sam.sample@example .com"), (118, "+1 555 123 4567")])
    page("link_tail", [(104, "linkedin.com/in/sam-sample-12a45 9876543210 sam.sample@example.com")])
    page("wrapped_phone", [(104, "sam.sample@example.com Mobile: +1 (555)"), (140, "123 4567")])
    page("wrapped_number_label", [(104, "Email: sam.sample@example.com"), (118, "Contact Number: 555-"), (150, "123-4567")])
    ema = stringWidth("Ema", "Helvetica", BODY)
    page("mailto_word_piece", [(104, "Email: write to me | Phone: 555 123 4567")],
         links=[("mailto:sam.sample@example.com", (ema, 104, stringWidth("Email:", "Helvetica", BODY)))])
    # An icon before the written address, linking to another mailbox.
    path = OUT.with_name("contact_icon_other_mailbox.pdf")
    c = Canvas(str(path), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.rect(X, H - 105, 9, 9, stroke=0, fill=1)
    c.linkURL("mailto:old.mailbox@example.org", (X - 1, H - 106, X + 10, H - 95), relative=0)
    c.setFont("Helvetica", BODY)
    c.drawString(X + 16, H - 104, "sam.sample@example.com")
    c.drawString(X, H - 140, "Experience")
    c.drawString(X, H - 158, "Platform engineer at Fabrikam, 2019 - 2024")
    c.showPage()
    c.save()
    print(f"wrote {path}")
    piece = "sam.sample"
    w = stringWidth(piece, "Helvetica", BODY)
    page("mailto_piece", [(104, "sam.sample@example.com")],
         links=[("mailto:sam.sample@example.com", (0, 104, w)),
                ("mailto:sample@example.com", (w, 104, stringWidth(piece + "@example.com", "Helvetica", BODY)))])


def job_headers():
    """Jobs whose header is bold body-size lines (company, dates, title) under
    the last bullet of the job above, which ends without a full stop."""
    c = Canvas(str(OUT.with_name("job_headers.pdf")), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.setFont("Helvetica-Bold", BODY)
    c.drawString(X, H - 110, "Professional Experience")
    y = H - 135
    jobs = [("Fabrikam Systems Pvt Ltd", "Jan 2021 - Present", "Senior Platform Engineer"),
            ("Contoso Retail", "Jun 2017 - Dec 2020", "Platform Engineer"),
            ("Northwind Traders", "Jul 2014 - May 2017", "Software Engineer")]
    for company, dates, title in jobs:
        for line in (company, dates, title):
            c.setFont("Helvetica-Bold", BODY)
            c.drawString(X, y, line)
            y -= 14
        c.setFont("Helvetica", BODY)
        for item in ["Ran the build and release tooling for six teams", "Cut deploy time from an hour to ten minutes"]:
            c.circle(X + 3, y + 3.5, 1.8, stroke=0, fill=1)
            c.drawString(X + 12, y, item)
            y -= 14
        y -= 6
    c.showPage()
    c.save()
    print("wrote job_headers.pdf")


def job_dates_right():
    """Job headers with the dates set to the right of the company, the end
    date wrapped back to the left margin, and bullets that stop well short
    of the dates (so the dates look like a column of their own)."""
    c = Canvas(str(OUT.with_name("job_dates_right.pdf")), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.setFont("Helvetica-Bold", BODY)
    c.drawString(X, H - 110, "Professional Experience")
    y = H - 135
    for company, start, end, title in [("Fabrikam Systems, Pune, MH", "Apr 2020 -", "Jun 2022", "Senior Platform Engineer"),
                                       ("Contoso Retail, Austin, TX", "Feb 2018 -", "Mar 2020", "Platform Engineer"),
                                       ("Northwind Traders, Leeds", "Jul 2014 -", "Jan 2018", "Software Engineer")]:
        c.setFont("Helvetica-Bold", BODY)
        if company.startswith("Northwind"):
            # A header in two pieces: "Lead role at" and the company.
            c.drawString(X, y, "Lead role at")
            c.drawString(X + 140, y, company)
        else:
            c.drawString(X, y, company)
        c.drawRightString(W - X, y, start)
        c.drawString(X, y - 14, end)
        c.drawString(X, y - 28, title)
        y -= 42
        c.setFont("Helvetica", BODY)
        for item in ["Ran the build and release tooling", "Cut deploy time to ten minutes", "Moved builds to a shared cache"]:
            c.circle(X + 3, y + 3.5, 1.8, stroke=0, fill=1)
            c.drawString(X + 12, y, item)
            y -= 14
        y -= 8
    c.showPage()
    c.save()
    print("wrote job_dates_right.pdf")


def personal_details():
    """An address with its city and PIN, a UPI ID and two referees under a
    References heading (one without a title). All invented."""
    path = OUT.with_name("personal_details.pdf")
    c = Canvas(str(path), pagesize=A4)
    c.setFont("Helvetica-Bold", 18)
    c.drawString(X, H - 80, "Sam Sample")
    c.setFont("Helvetica", BODY)
    c.drawString(X, H - 104, "sam.sample@example.com | +91 98765 43210")
    c.drawString(X, H - 118, "Address: 12, 3rd Cross, Indiranagar, Bengaluru 560038")
    c.drawString(X, H - 132, "UPI: samsample@okhdfc")
    y = H - 170
    for title, line in [("Experience", "Platform engineer at Fabrikam, Pune, 2019 - 2024"),
                        ("References", None)]:
        c.setFont("Helvetica-Bold", 13)
        c.drawString(X, y, title)
        c.setFont("Helvetica", BODY)
        y -= 20
        if line:
            c.drawString(X, y, line)
            y -= 30
    for entry in ["Anil Mehta, Engineering Manager, Fabrikam", "Dr. Priya Nair, Professor, Example Institute"]:
        c.drawString(X, y, entry)
        y -= 24
    c.showPage()
    c.save()
    print(f"wrote {path}")


if __name__ == "__main__":
    main()
    spaced()
    badges()
    links()
    form_bullets()
    section_in_table()
    names()
    unread_images()
    contacts()
    job_headers()
    job_dates_right()
    personal_details()
