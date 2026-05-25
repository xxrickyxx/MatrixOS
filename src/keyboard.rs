use x86_64::instructions::port::Port;

pub struct Keyboard {
    data_port:   Port<u8>,
    status_port: Port<u8>,
    pub shift:   bool,
}

// Sentinel control characters returned for non-printable / special keys.
// Chosen so they're distinct from anything else `read_line` cares about.
pub const KEY_F1:    char = '\x11';
pub const KEY_F2:    char = '\x12';
pub const KEY_F3:    char = '\x13';
pub const KEY_F4:    char = '\x14';
pub const KEY_F5:    char = '\x15';
pub const KEY_F6:    char = '\x16';
pub const KEY_ESC:   char = '\x1B';
pub const KEY_TAB:   char = '\t';

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            data_port:   Port::new(0x60),
            status_port: Port::new(0x64),
            shift:       false,
        }
    }

    pub fn read_scancode(&mut self) -> Option<u8> {
        unsafe {
            let status = self.status_port.read();
            if status & 1 == 1 {
                Some(self.data_port.read())
            } else {
                None
            }
        }
    }

    /// Track shift state.  Returns Some(char) for printable / handled keys
    /// (including F-key sentinels above), None otherwise.
    pub fn scancode_to_char(&mut self, scancode: u8) -> Option<char> {
        // Shift make/break
        match scancode {
            0x2A | 0x36 => { self.shift = true;  return None; }
            0xAA | 0xB6 => { self.shift = false; return None; }
            _ => {}
        }

        let shift = self.shift;
        match scancode {
            // Letters — shift toggles case
            0x1E => Some(if shift {'A'} else {'a'}),
            0x30 => Some(if shift {'B'} else {'b'}),
            0x2E => Some(if shift {'C'} else {'c'}),
            0x20 => Some(if shift {'D'} else {'d'}),
            0x12 => Some(if shift {'E'} else {'e'}),
            0x21 => Some(if shift {'F'} else {'f'}),
            0x22 => Some(if shift {'G'} else {'g'}),
            0x23 => Some(if shift {'H'} else {'h'}),
            0x17 => Some(if shift {'I'} else {'i'}),
            0x24 => Some(if shift {'J'} else {'j'}),
            0x25 => Some(if shift {'K'} else {'k'}),
            0x26 => Some(if shift {'L'} else {'l'}),
            0x32 => Some(if shift {'M'} else {'m'}),
            0x31 => Some(if shift {'N'} else {'n'}),
            0x18 => Some(if shift {'O'} else {'o'}),
            0x19 => Some(if shift {'P'} else {'p'}),
            0x10 => Some(if shift {'Q'} else {'q'}),
            0x13 => Some(if shift {'R'} else {'r'}),
            0x1F => Some(if shift {'S'} else {'s'}),
            0x14 => Some(if shift {'T'} else {'t'}),
            0x16 => Some(if shift {'U'} else {'u'}),
            0x2F => Some(if shift {'V'} else {'v'}),
            0x11 => Some(if shift {'W'} else {'w'}),
            0x2D => Some(if shift {'X'} else {'x'}),
            0x15 => Some(if shift {'Y'} else {'y'}),
            0x2C => Some(if shift {'Z'} else {'z'}),

            // Number row — shift gives the US-layout symbol
            0x02 => Some(if shift {'!'} else {'1'}),
            0x03 => Some(if shift {'@'} else {'2'}),
            0x04 => Some(if shift {'#'} else {'3'}),
            0x05 => Some(if shift {'$'} else {'4'}),
            0x06 => Some(if shift {'%'} else {'5'}),
            0x07 => Some(if shift {'^'} else {'6'}),
            0x08 => Some(if shift {'&'} else {'7'}),
            0x09 => Some(if shift {'*'} else {'8'}),
            0x0A => Some(if shift {'('} else {'9'}),
            0x0B => Some(if shift {')'} else {'0'}),

            // Punctuation
            0x0C => Some(if shift {'_'} else {'-'}),
            0x0D => Some(if shift {'+'} else {'='}),
            0x33 => Some(if shift {'<'} else {','}),
            0x34 => Some(if shift {'>'} else {'.'}),
            0x35 => Some(if shift {'?'} else {'/'}),
            0x27 => Some(if shift {':'} else {';'}),
            0x28 => Some(if shift {'"'} else {'\''}),
            0x29 => Some(if shift {'~'} else {'`'}),
            0x1A => Some(if shift {'{'} else {'['}),
            0x1B => Some(if shift {'}'} else {']'}),
            0x2B => Some(if shift {'|'} else {'\\'}),

            // Whitespace / control
            0x39 => Some(' '),
            0x1C => Some('\n'),
            0x0E => Some('\x08'),  // Backspace
            0x0F => Some(KEY_TAB),
            0x01 => Some(KEY_ESC),

            // Function keys
            0x3B => Some(KEY_F1),
            0x3C => Some(KEY_F2),
            0x3D => Some(KEY_F3),
            0x3E => Some(KEY_F4),
            0x3F => Some(KEY_F5),
            0x40 => Some(KEY_F6),

            _ => None,
        }
    }
}
