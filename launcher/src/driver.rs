// Which graphics driver to ask for when the card's cannot compile shaders ahead.
//
// The shaders are compiled before the game is played only with pipeline libraries that
// link fast (VK_EXT_graphics_pipeline_library, shaders/src/pipeline.rs). Vulkan says what
// the installed driver does, not what a newer one for the same card would: the cards
// below are those whose maker's newest Windows driver lacks them, so no update helps.
// Any other card's newer driver has them, and on Windows the launcher asks for it at
// every start, with the maker's download page. The families are told by PCI id, the
// ranges taken from pci.ids; which family has what, from the Vulkan Hardware Database's
// reports of each one's newest driver (vulkan.gpuinfo.org).
#![deny(unsafe_code)]

use ash::vk;

const NVIDIA: u32 = 0x10de;
const AMD: u32 = 0x1002;
const INTEL: u32 = 0x8086;

/// The graphics card the shaders are for: its name, PCI ids and driver.
#[derive(Clone, Default)]
pub struct Card {
    pub name: Vec<u8>,
    pub vendor: u32,
    pub pci: u32,
    /// Its driver's VkDriverId; 0 when the device is older than Vulkan 1.2.
    pub driver: vk::DriverId,
}

// Families whose newest Windows driver has no pipeline libraries, by vendor and PCI
// device id range (inclusive); ranges hold no id of another family that pci.ids knows.
const NO_NEWER_DRIVER: &[(u32, u32, u32)] = &[
    // NVIDIA Kepler (GK104, GK106, GK107, GK110, GK208): its last driver, 475.14, has
    // Vulkan 1.2.175, older than the extension.
    (NVIDIA, 0x0fc0, 0x103f),
    (NVIDIA, 0x1180, 0x11ff),
    (NVIDIA, 0x1280, 0x130b),
    // AMD GCN 1 to 3 (last driver 22.6.1, then 23.x builds of the same Vulkan 1.2.178),
    // and Polaris and Vega, on the package of their own whose Vulkan driver (2.0.279)
    // stopped before 2.0.299, AMD's first with the extension (24.2.1).
    (AMD, 0x6600, 0x66af), // Oland, Mars, Opal, Bonaire, Saturn, Tobago, Sun, Jet, Vega 20
    (AMD, 0x6780, 0x67ff), // Tahiti, Malta, Hawaii, Vesuvius, Ellesmere, Baffin
    (AMD, 0x6800, 0x683f), // Pitcairn, Wimbledon, Neptune, Curacao, Cape Verde, Venus, Heathrow, Chelsea
    (AMD, 0x6860, 0x687f), // Vega 10
    (AMD, 0x6900, 0x69af), // Topaz, Meso, Amethyst, Tonga, Polaris 22, Lexa, Polaris 12, Vega 12
    (AMD, 0x7300, 0x730f), // Fiji
    // AMD's integrated GCN: Kaveri, Kabini, Temash, Mullins, Carrizo, Stoney, and the
    // Vega of Ryzen's Raven Ridge, Picasso, Barcelo, Renoir, Cezanne and Lucienne, which
    // the Polaris and Vega package serves.
    (AMD, 0x1304, 0x131d),
    (AMD, 0x9830, 0x985f),
    (AMD, 0x9874, 0x9877),
    (AMD, 0x98e4, 0x98e4),
    (AMD, 0x15d8, 0x15d8),
    (AMD, 0x15dd, 0x15dd),
    (AMD, 0x15e7, 0x15e7),
    (AMD, 0x1636, 0x1636),
    (AMD, 0x1638, 0x1638),
    (AMD, 0x164c, 0x164c),
    // Intel's graphics before Xe, generations 9 to 11: the legacy driver (101.2xxx),
    // Vulkan 1.3.215, has none.
    (INTEL, 0x1900, 0x193f), // Skylake
    (INTEL, 0x5900, 0x593f), // Kaby Lake
    (INTEL, 0x87c0, 0x87ca), // Amber Lake
    (INTEL, 0x3e90, 0x3eaf), // Coffee Lake, Whiskey Lake
    (INTEL, 0x9b00, 0x9bff), // Comet Lake
    (INTEL, 0x5a84, 0x5a85), // Apollo Lake
    (INTEL, 0x3184, 0x3185), // Gemini Lake
    (INTEL, 0x8a50, 0x8a71), // Ice Lake
    (INTEL, 0x4e50, 0x4e71), // Jasper Lake
    (INTEL, 0x4540, 0x4571), // Elkhart Lake
];

// The card's maker's driver, or one too old to say (before Vulkan 1.2): not Mesa's, which
// a Windows build meets under Wine, and which has its own list of cards.
fn makers_driver(card: &Card) -> bool {
    let makers =
        [vk::DriverId::NVIDIA_PROPRIETARY, vk::DriverId::AMD_PROPRIETARY, vk::DriverId::INTEL_PROPRIETARY_WINDOWS];
    card.driver == vk::DriverId::from_raw(0) || makers.contains(&card.driver)
}

/// No newer driver for this card compiles shaders ahead: a Windows driver of the maker's,
/// for a card of a family above.
pub fn no_newer_driver(card: &Card) -> bool {
    no_newer_driver_on(cfg!(windows), card)
}

/// The warning is given at every start, not only when the shaders are looked at anew:
/// on Windows, where the driver is the maker's to download, when a newer one helps.
pub fn warn_every_start(card: &Card) -> bool {
    every_start_on(cfg!(windows), card)
}

/// The maker's driver download page, when the warning is given at every start.
pub fn download_page(card: &Card) -> Option<&'static str> {
    page_on(cfg!(windows), card)
}

fn no_newer_driver_on(windows: bool, card: &Card) -> bool {
    windows
        && makers_driver(card)
        && NO_NEWER_DRIVER
            .iter()
            .any(|&(vendor, first, last)| card.vendor == vendor && (first..=last).contains(&card.pci))
}

fn every_start_on(windows: bool, card: &Card) -> bool {
    windows && makers_driver(card) && !no_newer_driver_on(windows, card)
}

fn page_on(windows: bool, card: &Card) -> Option<&'static str> {
    if !every_start_on(windows, card) {
        return None;
    }
    match card.vendor {
        NVIDIA => Some("https://www.nvidia.com/en-us/drivers/"),
        AMD => Some("https://www.amd.com/en/support/download/drivers.html"),
        INTEL => Some("https://www.intel.com/content/www/us/en/support/detect.html"),
        _ => None,
    }
}

/// Opens `page` in the player's browser.
#[allow(unsafe_code)]
pub fn open(page: &str) -> bool {
    let page = std::ffi::CString::new(page).unwrap_or_default();
    // SAFETY: a C string.
    unsafe { sdl3_sys::everything::SDL_OpenURL(page.as_ptr()) }
}
