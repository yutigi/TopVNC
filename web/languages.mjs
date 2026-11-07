const LATIN = ['q w e r t y u i o p', 'a s d f g h j k l', 'z x c v b n m'].map(row => row.split(' '));
const THAI = ['ๆ ไ ำ พ ะ ั ี ร น ย บ ล', 'ฟ ห ก ด เ ้ ่ า ส ว ง', 'ผ ป แ อ ิ ื ท ม ใ ฝ'].map(row => row.split(' '));
export const THAI_SHIFT = ['๐ " ฎ ฑ ธ ํ ๊ ณ ฯ ญ ฐ ,', 'ฤ ฆ ฏ โ ฌ ็ ๋ ษ ศ ซ .', '( ) ฉ ฮ ฺ ์ ? ฒ ฬ ฦ'].map(row => row.split(' '));
export function defaultLanguages() {
  return { selected: 'en', profiles: [
    ['en', 'EN', 'English'], ['th', 'ไทย', 'Thai'], ['zh', '中文', 'Chinese'], ['ja', '日本語', 'Japanese'],
  ].map(([id, label, name]) => ({ id, label, name, tag: id, pinned: true })) };
}
export function validRows(rows) {
  return Array.isArray(rows) && rows.length === 3 && rows.every(row => Array.isArray(row) && row.length >= 3 && row.length <= 14 && row.every(key => typeof key === 'string' && key.trim().length > 0 && key.length <= 8 && !/[\u0000-\u001f\u007f]/u.test(key)));
}
export function profileRows(profile) { return profile.rows ?? (profile.id === 'th' ? THAI : LATIN); }
export function normalizeLanguages(value) {
  const fallback = defaultLanguages();
  const bounded = (s, max) => typeof s === 'string' && s.trim().length > 0 && s.length <= max;
  const profiles = value?.profiles;
  if (!Array.isArray(profiles) || !profiles.length || profiles.length > 16 || !profiles.every(p => p && bounded(p.id, 64) && bounded(p.name, 40) && bounded(p.label, 40) && bounded(p.tag, 40) && typeof p.pinned === 'boolean') || new Set(profiles.map(p => p.id)).size !== profiles.length) return fallback;
  let pins = 0;
  return {
    selected: profiles.some(p => p.id === value.selected) ? value.selected : profiles[0].id,
    profiles: profiles.map(p => ({ id: p.id, name: p.name, label: p.label, tag: p.tag, pinned: p.pinned && ++pins <= 4, ...(validRows(p.rows) ? { rows: p.rows.map(row => [...row]) } : {}) })),
  };
}
export function readLanguages(storage) {
  try {
    const saved = storage?.getItem('topvnc.mobile.languages.v1') ?? storage?.getItem('topvnc-wireframe-v1');
    return saved && saved.length <= 65536 ? normalizeLanguages(JSON.parse(saved)) : defaultLanguages();
  } catch { return defaultLanguages(); }
}
