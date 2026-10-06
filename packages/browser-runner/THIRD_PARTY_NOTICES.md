# Interactive container dependencies

`Dockerfile.interactive` adds Ubuntu Noble packages at fixed distribution
versions: TigerVNC `1.13.1+dfsg-2build2` (`Xtigervnc` and
`tigervncpasswd`), websockify `0.10.0+dfsg1-5build2`, Openbox
`3.6.1-12build5`, xauth `1:1.1.2-1build1`, Noto CJK
`1:20230817+repack1-3`, and Noto Color Emoji `2.042-1`. Chromium and
Playwright come from the pinned Playwright `1.63.0` image/package.

These are independent upstream components, not repository-owned code.
The distributed image keeps Ubuntu's required copyright/license records in
`/usr/share/doc/*/copyright`; do not strip these records from a published
image. TigerVNC and Openbox include GPL-family code; publishing the image
requires meeting their corresponding source distribution obligations.
Playwright's npm lock records its Apache-2.0 license. Validate the final
image's actual package records and licenses before external distribution.
