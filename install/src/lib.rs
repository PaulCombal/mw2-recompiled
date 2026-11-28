// The retail disc and its title update as an install and a start read them: the game
// checks the image its executable decrypts to (xex, crypto), the launcher installs from
// a disc image or folder (xdvdfs) and an update package (stfs) and applies the update's
// patch to the disc's executables (patch, with the LZX decoder it needs, lzx), and
// cargo xtask takes the files a build is made from out of the same. The game gunzips
// its title's XLAST and the launcher inflates a fastfile with inflate; both read a custom
// map's .arena with arena. Bad input -- a
// damaged image, a package cut short -- is an error to report, never a panic.
#![deny(unsafe_code)]

pub mod arena;
pub mod crypto;
pub mod inflate;
pub mod lzx;
pub mod patch;
pub mod stfs;
pub mod xdvdfs;
pub mod xex;
