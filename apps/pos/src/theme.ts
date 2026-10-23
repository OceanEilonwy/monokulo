// Applies the site theme toggle's choice in the POS: the same System /
// Light / Dark preference the rest of the dashboard uses, saved to the merchant's account through the
// site's own `/theme` form endpoint, and applied here at once by
// setting `data-theme` on <html> (absent means follow the system).

import { siteBase, siteUrl } from '@monokulo/ui/site';

export type Theme = 'system' | 'light' | 'dark';

/** Applies `theme` now and saves it; resolves `false` if saving failed
 * (the page keeps the new look either way). */
export async function applyTheme(theme: Theme): Promise<boolean> {
  if (theme === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  // `next` is the page as Monokulo names it, without its base path.
  const next = location.pathname.slice(siteBase().length) || '/';
  const form = new URLSearchParams({ theme, next });
  try {
    const response = await fetch(siteUrl('/theme'), { method: 'POST', body: form, redirect: 'manual' });
    return response.type === 'opaqueredirect' || response.ok;
  } catch { return false; }
}
