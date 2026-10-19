// Test-only projection of Ratatui's cursor/erase output. Not a general terminal
// emulator: styles, terminal queries and OSC metadata do not alter text cells.
export class TerminalScreenFixture {
  constructor(rows = 40, columns = 120) {
    this.rows = rows; this.columns = columns; this.x = 0; this.y = 0;
    this.scrollTop = 0; this.scrollBottom = rows - 1;
    this.saved = [0, 0]; this.pending = ''; this.clear();
  }
  clear() { this.cells = Array.from({ length: this.rows }, () => Array(this.columns).fill(' ')); }
  lineFeed() {
    if (this.y === this.scrollBottom) {
      this.cells.splice(this.scrollTop, 1);
      this.cells.splice(this.scrollBottom, 0, Array(this.columns).fill(' '));
    } else this.y = Math.min(this.rows - 1, this.y + 1);
  }
  csi(parameters, command) {
    const args = parameters.replace(/^[?<>=!]/, '').split(';').map(n => Number(n) || 0);
    const n = args[0] || 1;
    switch (command) {
      case 'H': case 'f': this.y = n - 1; this.x = (args[1] || 1) - 1; break;
      case 'G': this.x = n - 1; break;
      case 'd': this.y = n - 1; break;
      case 'A': this.y -= n; break;
      case 'B': this.y += n; break;
      case 'C': this.x += n; break;
      case 'D': this.x -= n; break;
      case 'E': this.y += n; this.x = 0; break;
      case 'F': this.y -= n; this.x = 0; break;
      case 'J':
        for (let y = 0; y < this.rows; y++) for (let x = 0; x < this.columns; x++) {
          if (args[0] >= 2 || (args[0] === 1 ? y < this.y || y === this.y && x <= this.x
            : y > this.y || y === this.y && x >= this.x)) this.cells[y][x] = ' ';
        }
        break;
      case 'K': this.cells[this.y].fill(' ', args[0] === 0 ? this.x : 0, args[0] === 1 ? this.x + 1 : this.columns); break;
      case 's': this.saved = [this.x, this.y]; break;
      case 'u': [this.x, this.y] = this.saved; break;
      case 'r': {
        const top = (args[0] || 1) - 1, bottom = (args[1] || this.rows) - 1;
        if (top >= 0 && top < bottom && bottom < this.rows) {
          this.scrollTop = top; this.scrollBottom = bottom; this.x = 0; this.y = 0;
        }
        break;
      }
      case 'h': if (parameters === '?1049') this.clear(); break;
    }
    this.x = Math.max(0, Math.min(this.columns - 1, this.x));
    this.y = Math.max(0, Math.min(this.rows - 1, this.y));
  }
  write(text) {
    this.pending += text;
    let i = 0;
    while (i < this.pending.length) {
      const char = String.fromCodePoint(this.pending.codePointAt(i));
      if (char === '\x1b') {
        if (i + 1 === this.pending.length) break;
        const next = this.pending[i + 1];
        if (next === '[') {
          const end = this.pending.slice(i + 2).search(/[@-~]/);
          if (end < 0) break;
          const final = i + 2 + end;
          this.csi(this.pending.slice(i + 2, final), this.pending[final]); i = final + 1; continue;
        }
        if (next === ']') {
          const end = this.pending.slice(i + 2).match(/\x07|\x1b\\/);
          if (!end) break;
          i += 2 + end.index + end[0].length; continue;
        }
        if (next === '7') this.saved = [this.x, this.y];
        if (next === '8') [this.x, this.y] = this.saved;
        if (next === 'M') {
          if (this.y === this.scrollTop) {
            this.cells.splice(this.scrollBottom, 1);
            this.cells.splice(this.scrollTop, 0, Array(this.columns).fill(' '));
          } else this.y = Math.max(0, this.y - 1);
        }
        i += 2; continue;
      }
      i += char.length;
      if (char === '\r') { this.x = 0; continue; }
      if (char === '\n') { this.lineFeed(); continue; }
      if (char === '\b') { this.x = Math.max(0, this.x - 1); continue; }
      if (char === '\t') { this.x = Math.min(this.columns - 1, (Math.floor(this.x / 8) + 1) * 8); continue; }
      if (char.codePointAt(0) < 32 || char === '\x7f') continue;
      if (/\p{Mark}/u.test(char)) { if (this.x > 0) this.cells[this.y][this.x - 1] += char; continue; }
      const cp = char.codePointAt(0);
      const width = cp >= 0x1100 && (cp <= 0x115f || cp >= 0x2e80 && cp <= 0xa4cf
        || cp >= 0xac00 && cp <= 0xd7a3 || cp >= 0xf900 && cp <= 0xfaff
        || cp >= 0xfe10 && cp <= 0xfe6f || cp >= 0xff01 && cp <= 0xff60
        || cp >= 0xffe0 && cp <= 0xffe6 || cp >= 0x1f300) ? 2 : 1;
      if (this.x + width > this.columns) { this.x = 0; this.lineFeed(); }
      const row = this.cells[this.y];
      // Replacing either half of a wide glyph clears its other half, including
      // continuations just outside this write. Empty strings mean live tails.
      if (row[this.x] === '' && this.x > 0) row[this.x - 1] = ' ';
      if (row[this.x + width] === '') row[this.x + width] = ' ';
      row[this.x] = char;
      if (width === 2) row[this.x + 1] = '';
      this.x += width;
    }
    this.pending = this.pending.slice(i);
  }
  text() { return this.cells.map(row => row.join('').trimEnd()).join('\n'); }
}
