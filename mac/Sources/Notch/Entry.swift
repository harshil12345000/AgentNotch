import Foundation
import NotchCore

/// Entry point. The same binary is the app and the hook helper.
@main
enum Entry {
    @MainActor
    static func main() {
        let arguments = Array(CommandLine.arguments.dropFirst())
        if arguments.contains("--hook") || arguments.contains("--codex-notify") {
            HookClient.run(arguments)
            exit(0)
        }
        if CLI.handle(arguments) {
            exit(0)
        }
        AppMain.run(arguments)
    }
}
