use alloc::string::String;
use alloc::{boxed::Box, string::ToString};
use clint::{SifiveClintWrap, THeadClintWrap};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};
use reset::SifiveTestDeviceWrap;
use spin::Mutex;
use uart_xilinx::MmioUartAxiLite;

use crate::cfg::NUM_HART_MAX;
use crate::devicetree::*;
use crate::fail;
use crate::platform::clint::{MachineClintType, SIFIVE_CLINT_COMPATIBLE, THEAD_CLINT_COMPATIBLE};
use crate::platform::console::Uart16550Wrap;
use crate::platform::console::UartBflbWrap;
use crate::platform::console::{
    MachineConsoleType, UART16650U8_COMPATIBLE, UART16650U32_COMPATIBLE, UARTAXILITE_COMPATIBLE,
    UARTBFLB_COMPATIBLE,
};
use crate::platform::reset::SIFIVETEST_COMPATIBLE;
use crate::sbi::SBI;
use crate::sbi::console::SbiConsole;
use crate::sbi::extensions;
use crate::sbi::hsm::SbiHsm;
use crate::sbi::ipi::SbiIpi;
use crate::sbi::logger;
use crate::sbi::reset::SbiReset;
use crate::sbi::rfence::SbiRFence;
use crate::sbi::trap_stack;

mod clint;
mod console;
mod reset;

type BaseAddress = usize;

type CpuEnableList = [bool; NUM_HART_MAX];

pub struct BoardInfo {
    pub memory_range: Option<Range<usize>>,
    pub console: Option<(BaseAddress, MachineConsoleType)>,
    pub reset: Option<BaseAddress>,
    pub ipi: Option<(BaseAddress, MachineClintType)>,
    pub cpu_num: Option<usize>,
    pub cpu_enabled: Option<CpuEnableList>,
    pub model: String,
}

impl BoardInfo {
    pub const fn new() -> Self {
        BoardInfo {
            memory_range: None,
            console: None,
            reset: None,
            ipi: None,
            cpu_enabled: None,
            cpu_num: None,
            model: String::new(),
        }
    }
}

pub struct Platform {
    pub info: BoardInfo,
    pub sbi: SBI,
    pub ready: AtomicBool,
}

impl Platform {
    pub const fn new() -> Self {
        Platform {
            info: BoardInfo::new(),
            sbi: SBI::new(),
            ready: AtomicBool::new(false),
        }
    }

    pub fn init(&mut self, fdt_address: usize) {
        self.info_init(fdt_address);
        self.sbi_init();
        trap_stack::prepare_for_trap();
        self.ready.swap(true, Ordering::Release);
    }

    fn info_init(&mut self, fdt_address: usize) {
        let dtb = parse_device_tree(fdt_address).unwrap_or_else(fail::device_tree_format);
        let dtb = dtb.share();

        let root: serde_device_tree::buildin::Node = serde_device_tree::from_raw_mut(&dtb)
            .unwrap_or_else(fail::device_tree_deserialize_root);
        let tree: Tree = root.deserialize();

        // Get console device, init sbi console and logger
        self.sbi_find_and_init_console(&root);

        // Get ipi and reset device info
        let mut find_device = |node: &serde_device_tree::buildin::Node| {
            let info = get_compatible_and_range(node);
            if let Some(info) = info {
                let (compatible, regs) = info;
                let base_address = regs.start;
                for device_id in compatible.iter() {
                    // Initialize clint device.
                    if SIFIVE_CLINT_COMPATIBLE.contains(&device_id) {
                        if node.get_prop("clint,has-no-64bit-mmio").is_some() {
                            self.info.ipi = Some((base_address, MachineClintType::TheadClint));
                        } else {
                            self.info.ipi = Some((base_address, MachineClintType::SiFiveClint));
                        }
                    } else if THEAD_CLINT_COMPATIBLE.contains(&device_id) {
                        self.info.ipi = Some((base_address, MachineClintType::TheadClint));
                    }
                    // Initialize reset device.
                    if SIFIVETEST_COMPATIBLE.contains(&device_id) {
                        self.info.reset = Some(base_address);
                    }
                }
            }
        };
        root.search(&mut find_device);

        // Get memory info
        // TODO: More than one memory node or range?
        let memory_reg = tree
            .memory
            .iter()
            .next()
            .unwrap()
            .deserialize::<Memory>()
            .reg;
        let memory_range = memory_reg.iter().next().unwrap().0;
        self.info.memory_range = Some(memory_range);

        // Get cpu number info
        self.info.cpu_num = Some(tree.cpus.cpu.len());

        // Get model info
        if let Some(model) = tree.model {
            let model = model.iter().next().unwrap_or("<unspecified>");
            self.info.model = model.to_string();
        } else {
            let model = "<unspecified>";
            self.info.model = model.to_string();
        }

        // TODO: Need a better extension initialization method
        extensions::init(&tree.cpus.cpu);

        // Find which hart is enabled by fdt
        let mut cpu_list: CpuEnableList = [false; NUM_HART_MAX];
        for cpu_iter in tree.cpus.cpu.iter() {
            let cpu = cpu_iter.deserialize::<Cpu>();
            let hart_id = cpu.reg.iter().next().unwrap().0.start;
            if let Some(x) = cpu_list.get_mut(hart_id) {
                *x = true;
            }
        }
        self.info.cpu_enabled = Some(cpu_list);
    }

    fn sbi_init(&mut self) {
        self.sbi_ipi_init();
        self.sbi_hsm_init();
        self.sbi_reset_init();
        self.sbi_rfence_init();
    }

    fn sbi_find_and_init_console(&mut self, root: &serde_device_tree::buildin::Node) {
        //  Get console device info
        if let Some(stdout_path) = root.chosen_stdout_path() {
            if let Some(node) = root.find(stdout_path) {
                let info = get_compatible_and_range(&node);
                if let Some((compatible, regs)) = info {
                    for device_id in compatible.iter() {
                        if UART16650U8_COMPATIBLE.contains(&device_id) {
                            self.info.console = Some((regs.start, MachineConsoleType::Uart16550U8));
                        }
                        if UART16650U32_COMPATIBLE.contains(&device_id) {
                            self.info.console =
                                Some((regs.start, MachineConsoleType::Uart16550U32));
                        }
                        if UARTAXILITE_COMPATIBLE.contains(&device_id) {
                            self.info.console = Some((regs.start, MachineConsoleType::UartAxiLite));
                        }
                        if UARTBFLB_COMPATIBLE.contains(&device_id) {
                            self.info.console = Some((regs.start, MachineConsoleType::UartBflb));
                        }
                    }
                }
            }
        }

        // init console and logger
        self.sbi_console_init();
        logger::Logger::init().unwrap();
        info!("Hello RustSBI!");
    }

    fn sbi_console_init(&mut self) {
        if let Some((base, console_type)) = self.info.console {
            self.sbi.console = match console_type {
                MachineConsoleType::Uart16550U8 => Some(SbiConsole::new(Mutex::new(Box::new(
                    Uart16550Wrap::<u8>::new(base),
                )))),
                MachineConsoleType::Uart16550U32 => Some(SbiConsole::new(Mutex::new(Box::new(
                    Uart16550Wrap::<u32>::new(base),
                )))),
                MachineConsoleType::UartAxiLite => Some(SbiConsole::new(Mutex::new(Box::new(
                    MmioUartAxiLite::new(base),
                )))),
                MachineConsoleType::UartBflb => Some(SbiConsole::new(Mutex::new(Box::new(
                    UartBflbWrap::new(base),
                )))),
            };
        } else {
            self.sbi.console = None;
        }
    }

    fn sbi_reset_init(&mut self) {
        if let Some(base) = self.info.reset {
            self.sbi.reset = Some(SbiReset::new(Mutex::new(Box::new(
                SifiveTestDeviceWrap::new(base),
            ))));
        } else {
            self.sbi.reset = None;
        }
    }

    fn sbi_ipi_init(&mut self) {
        if let Some((base, clint_type)) = self.info.ipi {
            self.sbi.ipi = match clint_type {
                MachineClintType::SiFiveClint => Some(SbiIpi::new(
                    Mutex::new(Box::new(SifiveClintWrap::new(base))),
                    self.info.cpu_num.unwrap_or(NUM_HART_MAX),
                )),
                MachineClintType::TheadClint => Some(SbiIpi::new(
                    Mutex::new(Box::new(THeadClintWrap::new(base))),
                    self.info.cpu_num.unwrap_or(NUM_HART_MAX),
                )),
            };
        } else {
            self.sbi.ipi = None;
        }
    }

    fn sbi_hsm_init(&mut self) {
        // TODO: Can HSM work properly when there is no ipi device?
        if self.info.ipi.is_some() {
            self.sbi.hsm = Some(SbiHsm);
        } else {
            self.sbi.hsm = None;
        }
    }

    fn sbi_rfence_init(&mut self) {
        // TODO: Can rfence work properly when there is no ipi device?
        if self.info.ipi.is_some() {
            self.sbi.rfence = Some(SbiRFence);
        } else {
            self.sbi.rfence = None;
        }
    }

    pub fn print_board_info(&self) {
        info!("RustSBI version {}", rustsbi::VERSION);
        rustsbi::LOGO.lines().for_each(|line| info!("{}", line));
        info!("Initializing RustSBI machine-mode environment.");

        self.print_platform_info();
        self.print_cpu_info();
        self.print_device_info();
        self.print_memory_info();
        self.print_additional_info();
    }

    #[inline]
    fn print_platform_info(&self) {
        info!("{:<30}: {}", "Platform Name", self.info.model);
    }

    fn print_cpu_info(&self) {
        info!(
            "{:<30}: {:?}",
            "Platform HART Count",
            self.info.cpu_num.unwrap_or(0)
        );

        if let Some(cpu_enabled) = &self.info.cpu_enabled {
            let mut enabled_harts = [0; NUM_HART_MAX];
            let mut count = 0;
            for (i, &enabled) in cpu_enabled.iter().enumerate() {
                if enabled {
                    enabled_harts[count] = i;
                    count += 1;
                }
            }
            info!("{:<30}: {:?}", "Enabled HARTs", &enabled_harts[..count]);
        } else {
            warn!("{:<30}: Not Available", "Enabled HARTs");
        }
    }

    #[inline]
    fn print_device_info(&self) {
        self.print_clint_info();
        self.print_console_info();
        self.print_reset_info();
        self.print_hsm_info();
        self.print_rfence_info();
    }

    #[inline]
    fn print_clint_info(&self) {
        match self.info.ipi {
            Some((base, device)) => {
                info!(
                    "{:<30}: {:?} (Base Address: 0x{:x})",
                    "Platform IPI Device", device, base
                );
            }
            None => warn!("{:<30}: Not Available", "Platform IPI Device"),
        }
    }

    #[inline]
    fn print_console_info(&self) {
        match self.info.console {
            Some((base, device)) => {
                info!(
                    "{:<30}: {:?} (Base Address: 0x{:x})",
                    "Platform Console Device", device, base
                );
            }
            None => warn!("{:<30}: Not Available", "Platform Console Device"),
        }
    }

    #[inline]
    fn print_reset_info(&self) {
        if let Some(base) = self.info.reset {
            info!(
                "{:<30}: Available (Base Address: 0x{:x})",
                "Platform Reset Device", base
            );
        } else {
            warn!("{:<30}: Not Available", "Platform Reset Device");
        }
    }

    #[inline]
    fn print_memory_info(&self) {
        if let Some(memory_range) = &self.info.memory_range {
            info!(
                "{:<30}: 0x{:x} - 0x{:x}",
                "Memory range", memory_range.start, memory_range.end
            );
        } else {
            warn!("{:<30}: Not Available", "Memory range");
        }
    }

    #[inline]
    fn print_hsm_info(&self) {
        info!(
            "{:<30}: {}",
            "Platform HSM Device",
            if self.have_hsm() {
                "Available"
            } else {
                "Not Available"
            }
        );
    }

    #[inline]
    fn print_rfence_info(&self) {
        info!(
            "{:<30}: {}",
            "Platform RFence Device",
            if self.have_rfence() {
                "Available"
            } else {
                "Not Available"
            }
        );
    }

    #[inline]
    fn print_additional_info(&self) {
        if !self.ready.load(Ordering::Acquire) {
            warn!(
                "{:<30}: Platform initialization is not complete.",
                "Platform Status"
            );
        } else {
            info!(
                "{:<30}: Platform initialization complete and ready.",
                "Platform Status"
            );
        }
    }
}

#[allow(unused)]
impl Platform {
    pub fn have_console(&self) -> bool {
        self.sbi.console.is_some()
    }

    pub fn have_reset(&self) -> bool {
        self.sbi.reset.is_some()
    }

    pub fn have_ipi(&self) -> bool {
        self.sbi.ipi.is_some()
    }

    pub fn have_hsm(&self) -> bool {
        self.sbi.hsm.is_some()
    }

    pub fn have_rfence(&self) -> bool {
        self.sbi.rfence.is_some()
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

pub(crate) static mut PLATFORM: Platform = Platform::new();
