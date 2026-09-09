/**
 * Shared asciinema-player setup for the recorded koda demos.
 *
 * Everything the player needs to look like koda rather than like a generic
 * terminal lives here: the NEON palette (from src/theme.rs, via the CSS in
 * koda.css), the site's mono face, and the idle trimming that cuts the pauses
 * the recorder leaves between keystrokes.
 */
import * as AsciinemaPlayer from 'asciinema-player';
import 'asciinema-player/dist/bundle/asciinema-player.css';

type Player = { dispose: () => void };

const players = new WeakMap<HTMLElement, Player>();

function options(el: HTMLElement) {
  return {
    cols: Number(el.dataset.cols ?? 100),
    rows: Number(el.dataset.rows ?? 30),
    autoPlay: el.dataset.autoplay === 'yes',
    preload: true,
    loop: false,
    // The recorder waits between steps so a person can follow the screen;
    // beyond a second and a half that is just dead air to a viewer.
    idleTimeLimit: 1.5,
    theme: 'koda',
    fit: 'width',
    terminalFontFamily: "'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, monospace",
    terminalLineHeight: 1.32,
    // Show a frame from the middle rather than the opening one: at three
    // seconds most recordings are still mid intro-animation, which posters as
    // an empty screen.
    poster: `npt:${el.dataset.poster ?? '0:08'}`,
  };
}

/** Build the player for one `.koda-demo__screen`, replacing any it already has. */
export function mountDemo(el: HTMLElement, cast?: string) {
  const src = cast ?? el.dataset.cast;
  if (!src) return;
  players.get(el)?.dispose();
  el.replaceChildren();
  players.set(el, AsciinemaPlayer.create(src, el, options(el)) as Player);
}

/** Swap the recording shown in an existing window, as the gallery does. */
export function swapDemo(el: HTMLElement, cast: string, cols: number, rows: number) {
  el.dataset.cast = cast;
  el.dataset.cols = String(cols);
  el.dataset.rows = String(rows);
  el.dataset.autoplay = 'yes';
  mountDemo(el, cast);
}
