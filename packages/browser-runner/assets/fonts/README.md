# Report PDF font

`NotoSansCJKsc-Regular.otf` is the unmodified Noto Sans CJK SC Regular 2.004 font.
It is the repository's single source font for PDF export.

- Copyright: © 2014–2021 Adobe (http://www.adobe.com/).
- License: SIL Open Font License 1.1, reproduced in [OFL.txt](OFL.txt).
- Upstream: https://github.com/notofonts/noto-cjk
- Source revision: `f8d157532fbfaeda587e826d4cd5b21a49186f7c`
- Source path: `Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf`
- Upstream Git blob: `dc15562470b4f842321894787a0d066879ccff8b`
- Size: 16,437,364 bytes.
- SHA-256: `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b`

The server loads this fixed asset at startup. Subset requests contain Unicode
codepoints only and cannot select a font path. HarfBuzz retains original glyph
IDs so the resulting CFF font can be embedded with `subset:false` and
`features:{locl:false}`. Keep the OFL notice with the source and redistributed
subsets. The public web license remains at `apps/web/public/fonts/OFL.txt`.

`src/pdf-font-subset-closure.json` records this exact font's 129 encoded
default GSUB output aliases, generated with fontTools 4.62.0 by reversing its
best cmap and visiting the `ccmp`, `liga`, `calt`, `rlig` and `clig` lookups
and their contextual lookup references. The source digest is checked when
constructing the subsetter. The aliases preserve the cmap entries pdf-lib
needs for ToUnicode without retaining the entire source cmap or changing
shaping. FontTools is a metadata-generation/verification tool, not a runtime
dependency. Update the metadata and its tests when changing the source font.
