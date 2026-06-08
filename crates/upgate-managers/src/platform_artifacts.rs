pub fn is_platform_artifact_version(version: &str) -> bool {
    let Some((head, cpu)) = version.rsplit_once('-') else {
        return false;
    };
    let Some((_base, os)) = head.rsplit_once('-') else {
        return false;
    };
    is_platform_os(os) && is_platform_cpu(cpu)
}

const fn is_platform_os(value: &str) -> bool {
    matches!(
        value.as_bytes(),
        b"aix" | b"android" | b"darwin" | b"freebsd" | b"linux" | b"openbsd" | b"sunos" | b"win32"
    )
}

const fn is_platform_cpu(value: &str) -> bool {
    matches!(
        value.as_bytes(),
        b"arm"
            | b"arm64"
            | b"ia32"
            | b"loong64"
            | b"mips"
            | b"mipsel"
            | b"ppc"
            | b"ppc64"
            | b"riscv64"
            | b"s390"
            | b"s390x"
            | b"x64"
    )
}
