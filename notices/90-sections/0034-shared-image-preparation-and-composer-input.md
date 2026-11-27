### Shared image preparation and composer input

`crates/secode-image/` owns the bounded preparation and container-validation code
previously in `crates/secode-product/src/dogfood/tools/image/`. It retains the
OpenAI Codex image utility adaptation at `bb64b894fd67158be0f874872e6d7bfe717e7368`
and Grok Build image compression/container validation adaptation at
`2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (both Apache-2.0). Grapt adds bounded
clipboard RGBA preparation and shares this owner between image tools and explicit
user attachments. The existing upstream licenses and notices remain applicable.

R clipboard reading uses `arboard` 3.6.1 (MIT OR Apache-2.0). Image bytes are prepared
on the existing protocol worker; the UI receives attachment counts, not pixels.

The arboard Windows backend uses clipboard-win 5.4.2 and error-code 3.3.2 under
BSL-1.0; their notice ships in `licenses/clipboard-backend/LICENSE-BOOST`.
The objc2-encode 4.1.0, objc2-foundation 0.3.2 and objc2 0.6.3 crates declare MIT
but their packages and published source supply no copyright notice. Their files
under `licenses/` contain explicitly labeled standard-text fallbacks; no copyright
attribution is invented.

The fax 0.2.6 and fax_derive 0.2.0 archives and their exact upstream commit
declare MIT but supply no copyright or license file. The dependency register
includes labeled standard MIT text without an invented copyright notice.

