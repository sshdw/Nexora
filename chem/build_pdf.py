# -*- coding: utf-8 -*-
import re
from reportlab.lib.pagesizes import A4
from reportlab.lib.units import cm
from reportlab.lib import colors
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.enums import TA_JUSTIFY, TA_CENTER
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import (BaseDocTemplate, PageTemplate, Frame, Paragraph, Spacer,
                                Table, TableStyle, PageBreak, KeepTogether)

from content import BLOCKS, SUBTITLE, META

F = "/usr/share/fonts/truetype/dejavu/"
pdfmetrics.registerFont(TTFont("DJ", F + "DejaVuSans.ttf"))
pdfmetrics.registerFont(TTFont("DJ-B", F + "DejaVuSans-Bold.ttf"))
pdfmetrics.registerFontFamily("DJ", normal="DJ", bold="DJ-B", italic="DJ", boldItalic="DJ-B")

ACCENT = colors.HexColor("#0F5C4A")
ACCENT2 = colors.HexColor("#1B6E8C")
DARK = colors.HexColor("#1F2A2E")
GREY = colors.HexColor("#556066")
ORANGE = colors.HexColor("#B55A00")
BOXBG = colors.HexColor("#F3F8F0")
BOXBORDER = colors.HexColor("#B8D3AC")
NOTE_BG = colors.HexColor("#FFF4E2")

BODY = 9.6

st_h1 = ParagraphStyle("h1", fontName="DJ-B", fontSize=11.5, leading=15,
                       textColor=colors.white, backColor=ACCENT,
                       borderPadding=(5, 6, 6, 6), spaceBefore=13, spaceAfter=8,
                       leftIndent=0, keepWithNext=1)
st_h2 = ParagraphStyle("h2", fontName="DJ-B", fontSize=10.3, leading=13,
                       textColor=ACCENT2, spaceBefore=9, spaceAfter=3.5, keepWithNext=1)
st_p = ParagraphStyle("p", fontName="DJ", fontSize=BODY, leading=13.4, textColor=DARK,
                      alignment=TA_JUSTIFY, spaceAfter=4.5)
st_bul = ParagraphStyle("bul", parent=st_p, leftIndent=15, bulletIndent=3, spaceAfter=2.6)
st_num = ParagraphStyle("num", parent=st_p, leftIndent=17, bulletIndent=2, spaceAfter=2.6)
st_box = ParagraphStyle("box", fontName="DJ", fontSize=8.7, leading=12.2,
                        textColor=colors.HexColor("#143D33"))
st_note = ParagraphStyle("note", parent=st_p, fontSize=8.9, leading=12.4,
                         textColor=colors.HexColor("#6B4510"))
st_th = ParagraphStyle("th", fontName="DJ-B", fontSize=8.3, leading=11, textColor=colors.white)
st_td = ParagraphStyle("td", fontName="DJ", fontSize=8.0, leading=10.8, textColor=DARK)
st_tdb = ParagraphStyle("tdb", parent=st_td, fontName="DJ-B")

BOLD_RE = re.compile(r"\*\*(.+?)\*\*")


def md(text):
    return BOLD_RE.sub(r'<font color="#0F5C4A"><b>\1</b></font>', text)


def esc(text):
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def rich(text):
    return md(esc(text))


def full_table_width():
    return A4[0] - 3.7 * cm


def make_box(lines):
    flow = []
    for line in lines:
        if line == "":
            flow.append(Spacer(1, 5))
        else:
            flow.append(Paragraph(esc(line).replace(" ", "&nbsp;"), st_box))
    t = Table([[flow]], colWidths=[full_table_width()])
    t.setStyle(TableStyle([
        ("BACKGROUND", (0, 0), (-1, -1), BOXBG),
        ("BOX", (0, 0), (-1, -1), 0.8, BOXBORDER),
        ("LEFTPADDING", (0, 0), (-1, -1), 9),
        ("RIGHTPADDING", (0, 0), (-1, -1), 7),
        ("TOPPADDING", (0, 0), (-1, -1), 6),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 6),
    ]))
    return [t, Spacer(1, 5)]


def make_note(text):
    p = Paragraph('<font color="#B55A00"><b>! </b></font>' + rich(text), st_note)
    t = Table([[p]], colWidths=[full_table_width()])
    t.setStyle(TableStyle([
        ("BACKGROUND", (0, 0), (-1, -1), NOTE_BG),
        ("LINEBEFORE", (0, 0), (0, -1), 2.2, colors.HexColor("#E0A050")),
        ("LEFTPADDING", (0, 0), (-1, -1), 8),
        ("RIGHTPADDING", (0, 0), (-1, -1), 7),
        ("TOPPADDING", (0, 0), (-1, -1), 5),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 6),
    ]))
    return [t, Spacer(1, 7)]


def make_table(headers, rows, widths):
    data = [[Paragraph(esc(h), st_th) for h in headers]]
    for row in rows:
        data.append([Paragraph(esc(c), st_tdb if i == 0 else st_td) for i, c in enumerate(row)])
    t = Table(data, colWidths=[w * cm for w in widths], repeatRows=1)
    style = [
        ("BACKGROUND", (0, 0), (-1, 0), ACCENT),
        ("VALIGN", (0, 0), (-1, -1), "MIDDLE"),
        ("GRID", (0, 0), (-1, -1), 0.4, colors.HexColor("#CBD8D1")),
        ("LEFTPADDING", (0, 0), (-1, -1), 5),
        ("RIGHTPADDING", (0, 0), (-1, -1), 5),
        ("TOPPADDING", (0, 0), (-1, -1), 4),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 4),
    ]
    for i in range(1, len(data)):
        if i % 2 == 0:
            style.append(("BACKGROUND", (0, i), (-1, i), colors.HexColor("#F1F6F3")))
    t.setStyle(TableStyle(style))
    return [t, Spacer(1, 8)]


def cover():
    flow = [Spacer(1, 2.6 * cm)]
    flow.append(Paragraph("ХИМИЯ", ParagraphStyle(
        "t", fontName="DJ-B", fontSize=46, leading=52, textColor=ACCENT, alignment=TA_CENTER)))
    flow.append(Spacer(1, 6))
    flow.append(Paragraph("9-СЫНЫП · КОНСПЕКТ", ParagraphStyle(
        "s", fontName="DJ-B", fontSize=19, leading=24, textColor=ACCENT2, alignment=TA_CENTER)))
    flow.append(Spacer(1, 22))
    line = Table([[""]], colWidths=[6 * cm], rowHeights=[2])
    line.setStyle(TableStyle([("BACKGROUND", (0, 0), (-1, -1), ACCENT2)]))
    flow.append(line)
    flow.append(Spacer(1, 22))
    flow.append(Paragraph(esc(SUBTITLE), ParagraphStyle(
        "st", fontName="DJ-B", fontSize=16, leading=22, textColor=DARK, alignment=TA_CENTER)))
    flow.append(Spacer(1, 10))
    flow.append(Paragraph("Оксидтер · Негіздер · Қышқылдар · Тұздар", ParagraphStyle(
        "o", fontName="DJ", fontSize=11.5, leading=16, textColor=ORANGE, alignment=TA_CENTER)))
    flow.append(Spacer(1, 34))
    for m in META:
        flow.append(Paragraph(esc(m), ParagraphStyle(
            "m", fontName="DJ", fontSize=8.8, leading=14, textColor=GREY, alignment=TA_CENTER)))
    flow.append(Spacer(1, 26))

    rows = [
        ("1", "Периодтық жүйе заңдылықтары"),
        ("2", "Оксидтер"),
        ("3", "Негіздер"),
        ("4", "Қышқылдар"),
        ("5", "Тұздар"),
        ("6–7", "Генетикалық байланыс · Иондық теңдеулер"),
        ("8", "Зертханалық тәжірибелер"),
        ("9–10", "Жиі жіберілетін қателер · Мини-тест"),
    ]
    data = [[Paragraph(a, ParagraphStyle("c1", fontName="DJ-B", fontSize=8.8, textColor=ACCENT, alignment=TA_CENTER)),
             Paragraph(b, st_td)] for a, b in rows]
    t = Table(data, colWidths=[2.2 * cm, 11.5 * cm], hAlign="CENTER")
    t.setStyle(TableStyle([
        ("GRID", (0, 0), (-1, -1), 0.4, colors.HexColor("#D6E2DA")),
        ("LEFTPADDING", (0, 0), (-1, -1), 6),
        ("TOPPADDING", (0, 0), (-1, -1), 4),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 4),
        ("BACKGROUND", (0, 0), (-1, -1), colors.HexColor("#F7FBF8")),
    ]))
    flow.append(t)
    flow.append(Spacer(1, 4.6 * cm))
    flow.append(Paragraph("Негізгі мазмұн НИШ 9-сынып бағдарламасының 9.1В және 9.4 бөлімдері бойынша жинақталған",
                          ParagraphStyle("f", fontName="DJ", fontSize=7.8, leading=11,
                                         textColor=GREY, alignment=TA_CENTER)))
    flow.append(PageBreak())
    return flow


def build(path):
    doc = BaseDocTemplate(path, pagesize=A4,
                          leftMargin=1.85 * cm, rightMargin=1.85 * cm,
                          topMargin=1.55 * cm, bottomMargin=1.5 * cm,
                          title="Химия 9-сынып конспектісі",
                          author="Arena AI")
    frame = Frame(doc.leftMargin, doc.bottomMargin, doc.width, doc.height, id="n",
                  leftPadding=0, rightPadding=0, topPadding=0, bottomPadding=0)

    def on_page(canv, d):
        canv.saveState()
        canv.setFont("DJ", 7.4)
        canv.setFillColor(GREY)
        canv.drawCentredString(A4[0] / 2, 1.0 * cm, f"Химия · 9-сынып конспектісі · {d.page}")
        canv.setStrokeColor(colors.HexColor("#DCE7E0"))
        canv.setLineWidth(0.6)
        canv.line(doc.leftMargin, 1.35 * cm, A4[0] - doc.rightMargin, 1.35 * cm)
        canv.restoreState()

    doc.addPageTemplates([PageTemplate(id="all", frames=[frame], onPage=on_page)])

    story = cover()
    for block in BLOCKS:
        kind = block[0]
        if kind == "h1":
            story.append(Paragraph(esc(block[1]), st_h1))
        elif kind == "h2":
            story.append(Paragraph(esc(block[1]), st_h2))
        elif kind == "p":
            story.append(Paragraph(rich(block[1]), st_p))
        elif kind == "bullets":
            for item in block[1]:
                story.append(Paragraph(rich(item), st_bul, bulletText="▪"))
            story.append(Spacer(1, 3))
        elif kind == "nums":
            for i, item in enumerate(block[1], 1):
                story.append(Paragraph(rich(item), st_num, bulletText=f"{i}."))
            story.append(Spacer(1, 3))
        elif kind == "note":
            story.extend(make_note(block[1]))
        elif kind == "box":
            story.extend(make_box(block[1]))
        elif kind == "table":
            story.extend(make_table(block[1], block[2], block[3]))
        elif kind == "pagebreak":
            story.append(PageBreak())

    doc.build(story)
    print("saved", path)


if __name__ == "__main__":
    build("Химия_9_сынып_конспект.pdf")
