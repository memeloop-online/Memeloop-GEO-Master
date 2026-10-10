# Report PDF font

`NotoSansCJKsc-Regular.otf` is the unmodified Noto Sans CJK SC Regular 2.004 font.

- Copyright: © 2014–2021 Adobe (http://www.adobe.com/).
- License: SIL Open Font License 1.1, reproduced in [OFL.txt](OFL.txt); the application Apache-2.0 license does not replace the font license.
- Upstream: https://github.com/notofonts/noto-cjk
- Source revision: `f8d157532fbfaeda587e826d4cd5b21a49186f7c`
- Source path: `Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf`
- Upstream Git blob: `dc15562470b4f842321894787a0d066879ccff8b`
- Size: 16,437,364 bytes.
- SHA-256: `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b`

The PDF exporter fetches this same-origin static asset only when exporting and embeds the full font. CFF subsetting with the pinned PDF libraries produced missing rendered Chinese glyphs despite extractable Unicode; full embedding preserves readable output at the cost of roughly 14 MB per PDF. No remote font service or locally installed system font is required. Generated documents are not subject to the font license merely because they use the font.
