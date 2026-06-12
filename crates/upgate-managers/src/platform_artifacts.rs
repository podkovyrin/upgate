pub fn is_platform_artifact_version(version: &str) -> bool {
    let Some((head, cpu)) = version.rsplit_once('-') else {
        return false;
    };
    let Some((_base, os)) = head.rsplit_once('-') else {
        return false;
    };
    is_platform_os(os) && is_platform_cpu(cpu)
}

fn is_platform_os(value: &str) -> bool {
    matches!(
        value,
        "aix" | "android" | "darwin" | "freebsd" | "linux" | "openbsd" | "sunos" | "win32"
    )
}

fn is_platform_cpu(value: &str) -> bool {
    matches!(
        value,
        "arm"
            | "arm64"
            | "ia32"
            | "loong64"
            | "mips"
            | "mipsel"
            | "ppc"
            | "ppc64"
            | "riscv64"
            | "s390"
            | "s390x"
            | "x64"
    )
}
