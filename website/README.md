# Postly project site

This is a dependency-free static showcase and documentation hub for Postly. It
is intentionally kept separate from the canonical Rust workspace. The public
project page is `https://othmaneblial.github.io/Postly/` and the guide index is
`https://othmaneblial.github.io/Postly/docs.html`.

Open index.html directly for a quick preview, or serve this directory locally:

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
