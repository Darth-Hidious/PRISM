import Foundation

struct PRISMCLIService {
    var executable: String
    var projectRoot: String
    var pythonPath: String

    func backendArguments() -> [String] {
        [
            "backend",
            "--project-root",
            projectRoot,
            "--python",
            pythonPath
        ]
    }
}

