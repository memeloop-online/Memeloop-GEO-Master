# Third-party notices for the web client

The web client uses noVNC `@novnc/novnc` version `1.7.0`, maintained at
https://github.com/novnc/noVNC and distributed under the Mozilla Public License
2.0 (MPL-2.0). Its package includes the license text at
`node_modules/@novnc/novnc/docs/LICENSE.MPL-2.0`, authors in `AUTHORS`, and
additional third-party license texts in `docs/LICENSE.*` and `vendor/pako/LICENSE`.
Source and license texts are available from the upstream repository at the
version of the package used in this build. Distribution of the bundled client
must preserve the applicable notices and offer the corresponding MPL-covered
source.

The knowledge editor uses `@tiptap/markdown` and
`@tiptap/extension-table` version `3.31.4` from
https://github.com/ueberdosis/tiptap, under the MIT license. Markdown parsing
also uses `marked` version `17.0.6` from https://github.com/markedjs/marked.
The distributed `public/EDITOR_THIRD_PARTY_NOTICES.txt` preserves the editor
license notices, including Marked's MIT notice and its bundled Markdown
copyright and redistribution notice. Exact package integrity values remain
in `pnpm-lock.yaml`.
