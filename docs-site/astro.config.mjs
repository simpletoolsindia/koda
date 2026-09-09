// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { kodaNeon, kodaSolarized } from './src/themes.mjs';

const repo = 'https://github.com/simpletoolsindia/koda';

export default defineConfig({
  site: 'https://simpletoolsindia.github.io',
  base: '/koda',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'koda',
      description:
        'koda is a single-binary terminal coding agent that drives local LLMs — Ollama, LM Studio, llama.cpp, vLLM, MLX — reads and edits your code, runs commands, and shows you a diff before it changes anything.',
      logo: { src: './src/assets/koda-mark.svg', alt: 'koda' },
      favicon: '/favicon.svg',
      customCss: [
        '@fontsource-variable/space-grotesk',
        '@fontsource/ibm-plex-mono/400.css',
        '@fontsource/ibm-plex-mono/500.css',
        '@fontsource/ibm-plex-mono/600.css',
        './src/styles/koda.css',
      ],
      social: [{ icon: 'github', label: 'GitHub', href: repo }],
      editLink: { baseUrl: `${repo}/edit/master/docs-site/` },
      lastUpdated: true,
      pagination: true,
      credits: false,
      expressiveCode: {
        themes: [kodaNeon, kodaSolarized],
        styleOverrides: {
          borderRadius: '0',
          borderWidth: '0',
          codeFontFamily: 'var(--koda-mono)',
          uiFontFamily: 'var(--koda-mono)',
          frames: { shadowColor: 'transparent' },
        },
      },
      head: [
        { tag: 'meta', attrs: { property: 'og:image', content: 'https://simpletoolsindia.github.io/koda/og.png' } },
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
        { tag: 'meta', attrs: { name: 'theme-color', content: '#0A0B1C' } },
      ],
      components: {
        Hero: './src/components/Hero.astro',
      },
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'What koda is', slug: 'introduction' },
            { label: 'See it work', slug: 'demos' },
            { label: 'Quickstart', slug: 'quickstart' },
            { label: 'Installation', slug: 'install' },
            { label: 'LLM providers', slug: 'providers' },
            { label: 'Configuration', slug: 'config' },
          ],
        },
        {
          label: 'Using koda',
          items: [
            { label: 'Modes & autonomy', slug: 'modes' },
            { label: 'Slash commands', slug: 'commands' },
            { label: 'Command line', slug: 'cli' },
            { label: 'Keyboard shortcuts', slug: 'keys' },
            { label: 'Tools & safety', slug: 'tools' },
            { label: 'Themes & appearance', slug: 'themes' },
          ],
        },
        {
          label: 'Capabilities',
          items: [
            { label: 'Overview', slug: 'features' },
            { label: 'Debugger', slug: 'debugger', badge: { text: 'new', variant: 'success' } },
            { label: 'Code graph', slug: 'codegraph' },
            { label: 'Skills & role agents', slug: 'skills' },
            { label: 'Memory & learning', slug: 'memory' },
            { label: 'Sessions & history', slug: 'sessions' },
            { label: 'Web search & fetch', slug: 'web' },
            { label: 'Browsing live pages', slug: 'browse' },
            { label: 'Watch mode', slug: 'watch' },
            { label: 'Files, images & OCR', slug: 'images' },
            { label: 'Custom tools', slug: 'customtools' },
            { label: 'Web control center', slug: 'webui' },
          ],
        },
        {
          label: 'How it works',
          items: [
            { label: 'Architecture', slug: 'architecture' },
            { label: 'Data flow', slug: 'dataflow' },
            { label: 'Tool-call protocols', slug: 'protocols' },
            { label: 'Prompt & context', slug: 'prompt' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Configuration keys', slug: 'reference/config-keys' },
            { label: 'Tool reference', slug: 'reference/tools' },
            { label: 'Troubleshooting', slug: 'troubleshooting' },
            { label: 'FAQ', slug: 'faq' },
          ],
        },
      ],
    }),
  ],
});
