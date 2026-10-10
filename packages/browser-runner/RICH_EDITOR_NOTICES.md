# Rich publication editor dependencies

The publication bridge uses Tiptap 3.31.4 and ProseMirror for schema, editing,
DOM parsing and serialization. Only the frozen publication/media mapping is
product-specific. esbuild 0.25.12 bundles the local browser test fixture.
These dependencies are MIT licensed; distribution retains their package
licenses and notices, including transitive dependency notices.

Sources:

- Tiptap: https://github.com/ueberdosis/tiptap
- ProseMirror: https://github.com/ProseMirror
- esbuild: https://github.com/evanw/esbuild

## MIT notices

Tiptap: Copyright (c) 2025, Tiptap GmbH

ProseMirror model: Copyright (C) 2015-2017 by Marijn Haverbeke
<marijn@haverbeke.berlin> and others

esbuild: Copyright (c) 2020 Evan Wallace

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
