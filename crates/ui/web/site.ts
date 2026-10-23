// Where the Monokulo serving this page starts its paths: `/monokulo` when a
// reverse proxy serves it under that path (stripping it before passing a
// request on), empty at the root of its host and over Tor. Every page says
// so in `<meta name="monokulo-base">` (monokulo's `crate::base`); a script
// making a URL of its own builds it with `siteUrl`, never a bare `/path`.
// Every other URL a script uses comes from the page, already made.

/** This Monokulo's base path, as the page says it. */
export function siteBase(): string {
  const meta = document.querySelector<HTMLMetaElement>('meta[name="monokulo-base"]');
  return meta ? meta.content : '';
}

/** `path`, a path on this Monokulo starting with `/`, as the URL to use. */
export function siteUrl(path: string): string {
  return siteBase() + path;
}
