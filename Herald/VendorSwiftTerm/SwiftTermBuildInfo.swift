// Stub for the SPM build plugin SwiftTermBuildInfoGenerator. The package
// generates this at build time from git state; vendored, we provide a
// minimal definition so XT-VERSION responds sensibly.
enum SwiftTermBuildInfo {
    static let tag = "vendored"
    static let branch = "vendored"
    static let version = "0.1.0"
}
