import test from 'node:test';
import assert from 'node:assert/strict';
import { defaultLanguages, normalizeLanguages, validRows, readLanguages, profileRows } from '../languages.mjs';

test('language profiles migrate locally, bound favorites and validate rows', () => {
  const saved = defaultLanguages(); saved.selected = 'th';
  saved.profiles.push({ id: 'custom', name: 'Other', label: 'OT', tag: 'und', pinned: true, rows: [['a', 'b', 'c'], ['d', 'e', 'f'], ['g', 'h', '🎮']] });
  const storage = { getItem: key => key === 'topvnc-wireframe-v1' ? JSON.stringify(saved) : null };
  const result = readLanguages(storage);
  assert.equal(result.selected, 'th'); assert.equal(result.profiles.filter(p => p.pinned).length, 4);
  assert.equal(result.profiles[4].rows[2][2], '🎮');
  result.profiles[4].rows[0][0] = 'X'; assert.equal(saved.profiles[4].rows[0][0], 'a');
});

test('malformed, duplicate, empty and oversized profiles fall back safely', () => {
  for (const value of [null, { profiles: [] }, { profiles: [null] }, { profiles: Array(17).fill(defaultLanguages().profiles[0]) }, { profiles: [defaultLanguages().profiles[0], defaultLanguages().profiles[0]] }]) assert.deepEqual(normalizeLanguages(value), defaultLanguages());
  assert.deepEqual(readLanguages({ getItem() { throw new Error('blocked'); } }), defaultLanguages());
  assert.deepEqual(readLanguages({ getItem: () => '{broken' }), defaultLanguages());
  const data = defaultLanguages(); data.profiles[0].rows = [['bad']];
  assert.equal(normalizeLanguages(data).profiles[0].rows, undefined);
  assert.equal(profileRows(data.profiles[1])[0][0], 'ๆ');
  assert.equal(validRows([['a', 'b', '\n'], ['d', 'e', 'f'], ['g', 'h', 'i']]), false);
});
