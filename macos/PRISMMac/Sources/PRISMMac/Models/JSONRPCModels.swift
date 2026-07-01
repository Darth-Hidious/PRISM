import Foundation

struct JSONRPCRequest: Encodable {
    let jsonrpc: String
    let method: String
    let id: Int
    let params: [String: AnyCodable]?

    init(method: String, id: Int, params: [String: AnyCodable]? = nil) {
        self.jsonrpc = "2.0"
        self.method = method
        self.id = id
        self.params = params
    }
}

struct AnyCodable: Encodable {
    let value: Any

    init(_ value: Any) {
        self.value = value
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch value {
        case let v as String: try container.encode(v)
        case let v as Int: try container.encode(v)
        case let v as Double: try container.encode(v)
        case let v as Bool: try container.encode(v)
        case let v as [Any]: try container.encode(v.map { AnyCodable($0) })
        case let v as [String: Any]: try container.encode(v.mapValues { AnyCodable($0) })
        case is NSNull: try container.encodeNil()
        default: try container.encodeNil()
        }
    }
}

struct JSONRPCResponse: Decodable {
    let jsonrpc: String?
    let id: Int?
    let result: [String: AnyCodableDecoded]?
    let error: RPCError?
}

struct RPCError: Decodable {
    let code: Int
    let message: String
}

struct JSONRPCNotification: Decodable {
    let jsonrpc: String?
    let method: String?
    let params: [String: AnyCodableDecoded]?
}

struct AnyCodableDecoded: Decodable {
    let value: Any

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let v = try? container.decode(String.self) { self.value = v }
        else if let v = try? container.decode(Int.self) { self.value = v }
        else if let v = try? container.decode(Double.self) { self.value = v }
        else if let v = try? container.decode(Bool.self) { self.value = v }
        else if let v = try? container.decode([AnyCodableDecoded].self) { self.value = v.map { $0.value } }
        else if let v = try? container.decode([String: AnyCodableDecoded].self) { self.value = v.mapValues { $0.value } }
        else if container.decodeNil() { self.value = NSNull() }
        else { self.value = NSNull() }
    }

    var stringValue: String? { value as? String }
    var intValue: Int? { value as? Int }
    var doubleValue: Double? { value as? Double }
    var boolValue: Bool? { value as? Bool }
    var dictValue: [String: Any]? { value as? [String: Any] }
    var arrayValue: [Any]? { value as? [Any] }
}

enum PRISMEvent {
    case welcome(version: String, toolCount: Int, sessionID: String)
    case textDelta(String)
    case textFlush
    case toolStart(name: String, callID: String, verb: String, preview: String?)
    case card(cardType: String, toolName: String, elapsedMs: Int, content: String)
    case prompt(message: String, toolName: String, choices: [String])
    case cost(inputTokens: Int, outputTokens: Int, turnCost: Double, sessionCost: Double)
    case turnComplete
    case status(autoApprove: Bool, messageCount: Int, model: String?)
    case view(title: String, tabs: [ViewTab])
    case error(String)

    struct ViewTab {
        let id: String
        let title: String
        let body: String
    }
}