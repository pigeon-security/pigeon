//! Runner classes. A job's needs map to a class, such as `linux-amd64-small`; a connector offers
//! classes, each with its real limits; a connector covers a job when one of its classes is the
//! job's or larger, of the same OS, arch, docker and gpu, and its limits hold the job.

use serde::{Deserialize, Serialize};

use super::config::{Arch, Needs, Network, Os};

/// A runner's size, smallest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Small,
    Medium,
    Large,
    Xlarge,
}

impl Size {
    fn as_str(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Medium => "medium",
            Size::Large => "large",
            Size::Xlarge => "xlarge",
        }
    }
}

fn os_str(os: Os) -> &'static str {
    match os {
        Os::Linux => "linux",
        Os::Macos => "macos",
        Os::Windows => "windows",
    }
}

fn arch_str(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "amd64",
        Arch::Arm64 => "arm64",
    }
}

fn with(on: bool) -> &'static str {
    if on { "with" } else { "without" }
}

/// A runner class: platform, size and features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Class {
    pub os: Os,
    pub arch: Arch,
    pub size: Size,
    pub docker: bool,
    pub gpu: bool,
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}-{}-{}",
            os_str(self.os),
            arch_str(self.arch),
            self.size.as_str()
        )?;
        if self.docker {
            write!(f, "-docker")?;
        }
        if self.gpu {
            write!(f, "-gpu")?;
        }
        Ok(())
    }
}

impl Class {
    /// The class's name, such as `linux-amd64-medium-docker`.
    pub fn name(&self) -> String {
        self.to_string()
    }
}

/// The smallest size that holds `cpu` and `memory_gb`.
pub fn size_for(cpu: u32, memory_gb: u32) -> Option<Size> {
    if cpu <= 2 && memory_gb <= 8 {
        Some(Size::Small)
    } else if cpu <= 4 && memory_gb <= 16 {
        Some(Size::Medium)
    } else if cpu <= 8 && memory_gb <= 32 {
        Some(Size::Large)
    } else if cpu <= 16 && memory_gb <= 64 {
        Some(Size::Xlarge)
    } else {
        None
    }
}

/// The class a job with `needs` asks for, or why there is none.
pub fn class_for(needs: &Needs) -> Result<Class, String> {
    let size = size_for(needs.cpu, needs.memory_gb).ok_or_else(|| {
        format!(
            "the job needs {} cpu and {} GB memory; no class is larger than 16 cpu and 64 GB",
            needs.cpu, needs.memory_gb
        )
    })?;
    Ok(Class {
        os: needs.os,
        arch: needs.arch,
        size,
        docker: needs.docker,
        gpu: needs.gpu,
    })
}

/// A class a connector offers, with its real limits, which may be below the size's nominal
/// ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offered {
    pub class: Class,
    pub cpu: u32,
    pub memory_gb: u32,
    pub disk_gb: u32,
    pub max_timeout_min: u32,
    /// Whether it can cut a job off the network.
    pub network_none: bool,
}

/// Whether `offered` can run a job with `needs` for `timeout_min`; the reason when not.
pub fn covers(offered: &Offered, needs: &Needs, timeout_min: u32) -> Result<(), String> {
    let want = class_for(needs)?;
    let c = &offered.class;
    let name = c.name();
    if c.os != want.os {
        return Err(format!(
            "{name} is {}, the job needs {}",
            os_str(c.os),
            os_str(want.os)
        ));
    }
    if c.arch != want.arch {
        return Err(format!(
            "{name} is {}, the job needs {}",
            arch_str(c.arch),
            arch_str(want.arch)
        ));
    }
    if c.docker != want.docker {
        return Err(format!(
            "{name} is {} docker, the job needs it {}",
            with(c.docker),
            with(want.docker)
        ));
    }
    if c.gpu != want.gpu {
        return Err(format!(
            "{name} is {} gpu, the job needs it {}",
            with(c.gpu),
            with(want.gpu)
        ));
    }
    if c.size < want.size {
        return Err(format!(
            "{name} is size {}, the job needs at least {}",
            c.size.as_str(),
            want.size.as_str()
        ));
    }
    if offered.cpu < needs.cpu {
        return Err(format!(
            "{name} has {} cpu, the job needs {}",
            offered.cpu, needs.cpu
        ));
    }
    if offered.memory_gb < needs.memory_gb {
        return Err(format!(
            "{name} has {} GB memory, the job needs {}",
            offered.memory_gb, needs.memory_gb
        ));
    }
    if offered.disk_gb < needs.disk_gb {
        return Err(format!(
            "{name} has {} GB disk, the job needs {}",
            offered.disk_gb, needs.disk_gb
        ));
    }
    if offered.max_timeout_min < timeout_min {
        return Err(format!(
            "{name} allows {} min, the job needs {}",
            offered.max_timeout_min, timeout_min
        ));
    }
    if needs.network == Network::None && !offered.network_none {
        return Err(format!("{name} cannot cut a job off the network"));
    }
    Ok(())
}

/// The smallest of `offers` that covers the job, ties to the earlier; when none does, every
/// offer's reason.
pub fn pick<'a>(
    offers: &'a [Offered],
    needs: &Needs,
    timeout_min: u32,
) -> Result<&'a Offered, String> {
    if offers.is_empty() {
        return Err("offers no classes".to_string());
    }
    let mut best: Option<&Offered> = None;
    let mut reasons = Vec::new();
    for o in offers {
        match covers(o, needs, timeout_min) {
            Ok(()) => {
                if best.is_none_or(|b| o.class.size < b.class.size) {
                    best = Some(o);
                }
            }
            Err(why) => reasons.push(why),
        }
    }
    best.ok_or_else(|| reasons.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(size: Size, cpu: u32, mem: u32) -> Offered {
        Offered {
            class: Class {
                os: Os::Linux,
                arch: Arch::Amd64,
                size,
                docker: false,
                gpu: false,
            },
            cpu,
            memory_gb: mem,
            disk_gb: 16,
            max_timeout_min: 360,
            network_none: false,
        }
    }

    fn needs() -> Needs {
        Needs::default()
    }

    fn why(o: &Offered, n: &Needs, t: u32) -> String {
        covers(o, n, t).unwrap_err()
    }

    #[test]
    fn names_carry_the_platform_size_and_features() {
        let mut c = offer(Size::Small, 2, 8).class;
        assert_eq!(c.name(), "linux-amd64-small");
        c.docker = true;
        assert_eq!(c.name(), "linux-amd64-small-docker");
        c.docker = false;
        c.gpu = true;
        c.size = Size::Medium;
        assert_eq!(c.name(), "linux-amd64-medium-gpu");
        let c = Class {
            os: Os::Linux,
            arch: Arch::Arm64,
            size: Size::Large,
            docker: true,
            gpu: true,
        };
        assert_eq!(c.to_string(), "linux-arm64-large-docker-gpu");
    }

    #[test]
    fn sizes_have_their_boundaries() {
        assert_eq!(size_for(2, 8), Some(Size::Small));
        assert_eq!(size_for(3, 8), Some(Size::Medium));
        assert_eq!(size_for(2, 9), Some(Size::Medium));
        assert_eq!(size_for(8, 32), Some(Size::Large));
        assert_eq!(size_for(16, 64), Some(Size::Xlarge));
        assert_eq!(size_for(17, 1), None);
        assert_eq!(size_for(1, 65), None);
    }

    #[test]
    fn the_default_job_is_linux_amd64_small_and_too_large_has_no_class() {
        assert_eq!(class_for(&needs()).unwrap().name(), "linux-amd64-small");
        assert!(class_for(&Needs { cpu: 32, ..needs() }).is_err());
    }

    #[test]
    fn each_mismatch_has_its_own_reason() {
        let small = offer(Size::Small, 2, 8);
        assert!(covers(&small, &needs(), 30).is_ok());
        let cases = [
            (
                Needs {
                    os: Os::Macos,
                    ..needs()
                },
                30,
                "macos",
            ),
            (
                Needs {
                    arch: Arch::Arm64,
                    ..needs()
                },
                30,
                "arm64",
            ),
            (
                Needs {
                    docker: true,
                    ..needs()
                },
                30,
                "docker",
            ),
            (
                Needs {
                    gpu: true,
                    ..needs()
                },
                30,
                "gpu",
            ),
            (Needs { cpu: 4, ..needs() }, 30, "size"),
            (
                Needs {
                    disk_gb: 32,
                    ..needs()
                },
                30,
                "disk",
            ),
            (needs(), 720, "min"),
            (
                Needs {
                    network: Network::None,
                    ..needs()
                },
                30,
                "network",
            ),
        ];
        for (n, t, word) in cases {
            let w = why(&small, &n, t);
            assert!(w.contains(word), "{word}: {w}");
        }
    }

    #[test]
    fn real_limits_below_the_nominal_size_refuse() {
        let n = Needs { cpu: 4, ..needs() };
        assert!(why(&offer(Size::Medium, 2, 16), &n, 30).contains("cpu"));
        let n = Needs {
            cpu: 4,
            memory_gb: 16,
            ..needs()
        };
        assert!(why(&offer(Size::Medium, 4, 12), &n, 30).contains("memory"));
    }

    #[test]
    fn a_larger_class_runs_a_smaller_job_never_the_reverse() {
        assert!(covers(&offer(Size::Medium, 4, 16), &needs(), 30).is_ok());
        assert!(covers(&offer(Size::Small, 2, 8), &Needs { cpu: 4, ..needs() }, 30).is_err());
    }

    #[test]
    fn pick_takes_the_smallest_that_covers() {
        let offers = [offer(Size::Medium, 4, 16), offer(Size::Small, 2, 8)];
        assert_eq!(pick(&offers, &needs(), 30).unwrap().class.size, Size::Small);
        let mut a = offer(Size::Small, 2, 8);
        a.disk_gb = 20;
        let offers = [a.clone(), offer(Size::Small, 2, 8)];
        assert_eq!(pick(&offers, &needs(), 30).unwrap(), &a);
    }

    #[test]
    fn pick_explains_every_refusal() {
        let offers = [offer(Size::Small, 2, 8), offer(Size::Medium, 4, 12)];
        let n = Needs {
            cpu: 4,
            memory_gb: 16,
            ..needs()
        };
        let w = pick(&offers, &n, 30).unwrap_err();
        assert!(
            w.contains("size") && w.contains("memory") && w.contains("; "),
            "{w}"
        );
        assert_eq!(pick(&[], &needs(), 30).unwrap_err(), "offers no classes");
    }
}
