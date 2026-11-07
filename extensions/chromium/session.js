'use strict';
// Collects the browser session that authorized a captured download. QDM applies
// these headers to that download only and keeps them in memory; they are never
// written to task history. Everything is best effort: a missing session simply
// leaves the URL to work as before.
(function (global) {
  const headerSafe = value => typeof value === 'string' && value.length > 0 && !/[\r\n\0]/.test(value);
  const clamp = (value, max) => (value.length > max ? value.slice(0, max) : value);

  async function browserSession(url, referer, limit = 16384) {
    const headers = {};
    const api = global.browser || global.chrome;
    if (!api || !api.cookies || typeof api.cookies.getAll !== 'function') return headers;
    try {
      const ua = global.navigator && global.navigator.userAgent;
      if (headerSafe(ua)) headers['User-Agent'] = clamp(ua, 512);
      const languages = Array.isArray(global.navigator && global.navigator.languages)
        ? global.navigator.languages.filter(language => typeof language === 'string' && /^[A-Za-z0-9-]{1,35}$/.test(language))
        : [];
      if (languages.length) headers['Accept-Language'] = languages.slice(0, 8).join(',');
      if (headerSafe(referer) && /^https?:\/\//i.test(referer)) headers['Referer'] = clamp(referer, 8192);
      const cookies = await api.cookies.getAll({ url });
      if (Array.isArray(cookies) && cookies.length) {
        const cookie = cookies
          .filter(entry => entry && headerSafe(entry.name) && typeof entry.value === 'string' &&
            !/[;=\r\n\0]/.test(entry.name) && !/[;\r\n\0]/.test(entry.value))
          .sort((left, right) => (right.path || '').length - (left.path || '').length)
          .map(entry => entry.name + '=' + entry.value)
          .join('; ');
        if (cookie) headers.Cookie = clamp(cookie, limit);
      }
    } catch (_) {
      // The download URL is still usable without the browser session.
    }
    return headers;
  }

  global.QDMSession = { browserSession };
})(globalThis);
