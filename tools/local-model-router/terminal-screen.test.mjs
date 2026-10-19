import test from 'node:test';
import assert from 'node:assert/strict';
import { TerminalScreenFixture } from './terminal-screen-fixture.mjs';

test('terminal projection combines changed cells instead of treating output as a transcript', () => {
  const screen = new TerminalScreenFixture(3, 80);
  screen.write('prompt kept xx queue');
  screen.write('\x1b[1;13Hin');
  assert.match(screen.text(), /prompt kept in queue/);
});
test('terminal projection retains partial CSI and OSC sequences across chunks', () => {
  const screen = new TerminalScreenFixture(3, 80);
  screen.write('old\x1b[2;'); screen.write('1H\x1b[32mApplied for this turn: model · high effort');
  screen.write('\x1b]10;rgb:ffff/ffff/ffff\x1b'); screen.write('\\\x1b[0m');
  assert.match(screen.text(), /Applied for this turn: model · high effort/);
  assert.ok(!screen.text().includes('rgb:'));
});
test('terminal projection handles Hangul cell width and line erase', () => {
  const screen = new TerminalScreenFixture(3, 40);
  screen.write('실행 설정표\x1b[1;12H / Jev\x1b[2;1Hold suffix');
  screen.write('\x1b[2;1Hnew\x1b[K');
  assert.equal(screen.text().split('\n')[0], '실행 설정표 / Jev');
  assert.equal(screen.text().split('\n')[1], 'new');
});
// Expected cells independently checked with MindPlayer's vt100 parser. The first
// sequence reduces the retained native F6 redraw without reverse-index output.
test('terminal projection clears an abandoned wide continuation when a wide glyph shifts left', () => {
  const screen = new TerminalScreenFixture(1, 8);
  screen.write('\x1b[1;2H송\x1b[1;1H송\x1b[1;4H동');
  assert.equal(screen.text(), '송 동');
  assert.deepEqual(screen.cells[0].slice(0, 5), ['송', '', ' ', '동', '']);
});
test('terminal projection clears the continuation when a narrow glyph replaces a wide head', () => {
  const screen = new TerminalScreenFixture(1, 8);
  screen.write('송\x1b[1;1HA\x1b[1;3HB');
  assert.equal(screen.text(), 'A B');
  assert.deepEqual(screen.cells[0].slice(0, 3), ['A', ' ', 'B']);
});
test('terminal projection clears the wide head when a narrow glyph replaces its continuation', () => {
  const screen = new TerminalScreenFixture(1, 8);
  screen.write('송\x1b[1;2HA');
  assert.equal(screen.text(), ' A');
  assert.deepEqual(screen.cells[0].slice(0, 2), [' ', 'A']);
});
test('terminal projection handles relative moves, saved cursor, scrolling and full erase', () => {
  const screen = new TerminalScreenFixture(2, 8);
  screen.write('A\x1b[s\x1b[2;1HB\x1b[u\x1b[1CC\r\nD\r\nE');
  assert.equal(screen.text(), 'D\nE');
  screen.write('\x1b[2J\x1b[HOK');
  assert.equal(screen.text(), 'OK\n');
});

test('terminal projection reverse index moves upward without changing the column', () => {
  const screen = new TerminalScreenFixture(3, 12);
  screen.write('ONE\x1b[2;1HTWO\x1b[3;1HTHREE\x1b[2;2H\x1bMX');
  assert.equal(screen.text(), 'OXE\nTWO\nTHREE');
});

test('terminal projection reverse index at the top scrolls existing rows down', () => {
  const screen = new TerminalScreenFixture(3, 12);
  screen.write('one\r\ntwo\r\nthree\x1b[1;2H\x1bMX');
  assert.equal(screen.text(), ' X\none\ntwo');
});

test('terminal projection handles reverse index fragmented across output chunks', () => {
  const screen = new TerminalScreenFixture(3, 12);
  screen.write('\x1b[3;1Hthird\x1b[2;1Hsecond\x1b');
  screen.write('M\rfirst');
  assert.equal(screen.text(), 'first\nsecond\nthird');
});

test('terminal projection reverse index scrolls only the configured region', () => {
  const screen = new TerminalScreenFixture(5, 12);
  screen.write('above\r\none\r\ntwo\r\nthree\r\nbelow\x1b[2;4r\x1b[2;2H\x1bMX');
  assert.equal(screen.text(), 'above\n X\none\ntwo\nbelow');
});

test('terminal projection line feed scrolls only the configured region', () => {
  const screen = new TerminalScreenFixture(5, 12);
  screen.write('above\r\none\r\ntwo\r\nthree\r\nbelow\x1b[2;4r\x1b[4;1H\r\nlast');
  assert.equal(screen.text(), 'above\ntwo\nthree\nlast\nbelow');
});

test('terminal projection resetting margins homes the cursor and restores full-screen scrolling', () => {
  const screen = new TerminalScreenFixture(3, 12);
  screen.write('one\r\ntwo\r\nthree\x1b[2;3r\x1b[rX\x1b[3;1H\r\nlast');
  assert.equal(screen.text(), 'two\nthree\nlast');
  screen.write('\x1b[rHOME');
  assert.equal(screen.text(), 'HOME\nthree\nlast');
});
