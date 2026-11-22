// Translocator for macOS: a SwiftUI and AppKit shell over the Rust core.
//
// `ssh` runs this same executable as its SSH_ASKPASS helper, with the
// askpass socket in the environment and the prompt as the first argument:
// then it only forwards the prompt to the running app and exits, before any
// UI exists.

import Foundation

if ProcessInfo.processInfo.environment[askpassSocketEnv()] != nil {
    let prompt = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ""
    exit(askpassClient(prompt: prompt))
}

TranslocatorApp.main()
