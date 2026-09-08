# Vendored browser libraries

Inlined into `dist/index.html` by `build.sh`, so the web UI works offline and
runs no third-party code fetched at page load.

| file | source | version |
| --- | --- | --- |
| `react.js` | `https://unpkg.com/react@18.3.1/umd/react.production.min.js` | 18.3.1 |
| `react-dom.js` | `https://unpkg.com/react-dom@18.3.1/umd/react-dom.production.min.js` | 18.3.1 |
| `tailwind.js` | `https://cdn.tailwindcss.com` (Play CDN) | 3.x |

To refresh one, download it again at a pinned version and re-run `./build.sh`.
`@babel/standalone` is deliberately absent: the JSX is transpiled at build time
by esbuild, which is what keeps this directory at ~540 KB rather than 3.4 MB.
