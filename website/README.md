# Postly project site

This is a dependency-free static showcase and documentation hub for Postly. It
is intentionally kept separate from the canonical Rust workspace. The public
project page is `https://othmaneblial.github.io/Postly/` and the guide index is
`https://othmaneblial.github.io/Postly/docs.html`.

Serve this directory over HTTP to test fonts, navigation and clipboard controls:

~~~bash
python3 -m http.server 4173 --directory website
~~~

The page links back to the repository's README and versioned documentation.
`robots.txt`, `sitemap.xml` and `llms.txt` keep the small public surface
crawlable and make the project's factual boundaries easy to discover.
Before each GitHub Pages update, verify the final base path, link targets,
mobile overflow, keyboard focus, reduced-motion behavior and browser console.
The site does not claim download counts, benchmark advantages or full Postman
parity.

The September 7 redesign uses bundled IBM Plex Sans (OFL license included),
the canonical Postly logo, and an actual native screenshot from the UI source
at `1130c32`. `assets/postly-dark.png` is an unretouched window capture of the
real local Orders API at 1280 logical pixels / 2x backing resolution. It is
not an HTML reconstruction. The displayed latency is one request, not a benchmark.

The landing page and docs explicitly distinguish the old public v0.1.0 archive
from the current 0.2.0 source preview. Update that status only after verifying
new public release assets. The final real-product video will be integrated
after the candidate build is fixed and captured.
