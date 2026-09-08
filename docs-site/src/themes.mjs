// The two palettes below are koda's own, lifted value-for-value from
// src/theme.rs: NEON (koda's default terminal theme) and SOLARIZED_LIGHT.
// Code on this site is highlighted with the same colours koda highlights
// code with in your terminal.

const neon = {
  text: '#E9ECFF',
  muted: '#7A82B4',
  accent: '#00F5D4',
  accentAlt: '#FF47CD',
  success: '#58FF9C',
  warning: '#FFC740',
  error: '#FF4A6E',
  info: '#54ADFF',
  violet: '#B294FF',
  surface: '#141834',
  bg: '#0A0B1C',
};

const solar = {
  text: '#586E75',
  muted: '#93A1A1',
  accent: '#268BD2',
  accentAlt: '#D33682',
  success: '#859900',
  warning: '#B58900',
  error: '#DC322F',
  info: '#2AA198',
  violet: '#6C71C4',
  surface: '#EEE8D5',
  bg: '#FDF6E3',
};

/** Build a Shiki/TextMate theme from one of koda's palettes. */
function build(name, type, p) {
  return {
    name,
    type,
    colors: {
      'editor.background': p.surface,
      'editor.foreground': p.text,
      'editor.selectionBackground': type === 'dark' ? '#34285C' : '#DBD6C4',
      'editorLineNumber.foreground': p.muted,
    },
    tokenColors: [
      { scope: ['comment', 'punctuation.definition.comment'], settings: { foreground: p.muted, fontStyle: 'italic' } },
      {
        scope: ['keyword', 'storage', 'storage.type', 'keyword.control', 'keyword.operator.new', 'variable.language'],
        settings: { foreground: p.accentAlt },
      },
      { scope: ['string', 'string.quoted', 'punctuation.definition.string', 'meta.embedded.line'], settings: { foreground: p.success } },
      { scope: ['constant.numeric', 'constant.language', 'constant.character.escape'], settings: { foreground: p.warning } },
      { scope: ['entity.name.function', 'support.function', 'meta.function-call'], settings: { foreground: p.accent } },
      { scope: ['entity.name.type', 'entity.name.class', 'support.type', 'support.class', 'entity.other.inherited-class'], settings: { foreground: p.violet } },
      { scope: ['variable', 'variable.other', 'meta.definition.variable'], settings: { foreground: p.text } },
      { scope: ['variable.parameter'], settings: { foreground: p.info } },
      { scope: ['entity.name.tag', 'punctuation.definition.tag'], settings: { foreground: p.accentAlt } },
      { scope: ['entity.other.attribute-name', 'support.type.property-name'], settings: { foreground: p.accent } },
      { scope: ['keyword.operator', 'punctuation'], settings: { foreground: p.muted } },
      { scope: ['markup.heading'], settings: { foreground: p.accent, fontStyle: 'bold' } },
      { scope: ['markup.inserted'], settings: { foreground: p.success } },
      { scope: ['markup.deleted'], settings: { foreground: p.error } },
      { scope: ['invalid'], settings: { foreground: p.error } },
    ],
  };
}

export const kodaNeon = build('koda-neon', 'dark', neon);
export const kodaSolarized = build('koda-solarized-light', 'light', solar);
