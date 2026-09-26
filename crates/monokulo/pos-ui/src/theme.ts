// The POS's theme toggle: the same System / Light / Dark preference the
// rest of the dashboard uses, saved to the merchant's account through the
// site's own `/dashboard/theme` form endpoint, and applied here at once by
// setting `data-theme` on <html> (absent means follow the system).

export type Theme = 'system' | 'light' | 'dark';

export function currentTheme(): Theme {
  const value = document.documentElement.dataset.theme;
  return value === 'light' || value === 'dark' ? value : 'system';
}

export function nextTheme(theme: Theme): Theme {
  return theme === 'system' ? 'light' : theme === 'light' ? 'dark' : 'system';
}

/** Applies `theme` now and saves it; resolves `false` if saving failed
 * (the page keeps the new look either way). */
export async function applyTheme(theme: Theme): Promise<boolean> {
  if (theme === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  const form = new URLSearchParams({ theme, next: location.pathname });
  try {
    const response = await fetch('/dashboard/theme', { method: 'POST', body: form, redirect: 'manual' });
    return response.type === 'opaqueredirect' || response.ok;
  } catch { return false; }
}
