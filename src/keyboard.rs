use x86_64::instructions::port::Port;

pub struct Keyboard {
    data_port: Port<u8>,
    status_port: Port<u8>,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            data_port: Port::new(0x60),
            status_port: Port::new(0x64),
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

    pub fn scancode_to_char(&self, scancode: u8) -> Option<char> {
        match scancode {
            0x1E => Some('A'), 0x30 => Some('B'), 0x2E => Some('C'), 0x20 => Some('D'),
            0x12 => Some('E'), 0x21 => Some('F'), 0x22 => Some('G'), 0x23 => Some('H'),
            0x17 => Some('I'), 0x24 => Some('J'), 0x25 => Some('K'), 0x26 => Some('L'),
            0x32 => Some('M'), 0x31 => Some('N'), 0x18 => Some('O'), 0x19 => Some('P'),
            0x10 => Some('Q'), 0x13 => Some('R'), 0x1F => Some('S'), 0x14 => Some('T'),
            0x16 => Some('U'), 0x2F => Some('V'), 0x11 => Some('W'), 0x2D => Some('X'),
            0x15 => Some('Y'), 0x2C => Some('Z'),
            0x02 => Some('1'), 0x03 => Some('2'), 0x04 => Some('3'), 0x05 => Some('4'),
            0x06 => Some('5'), 0x07 => Some('6'), 0x08 => Some('7'), 0x09 => Some('8'),
            0x0A => Some('9'), 0x0B => Some('0'),
            0x39 => Some(' '), 0x1C => Some('\n'), 0x0E => Some('\x08'), // Backspace
            _ => None,
        }
    }
}
