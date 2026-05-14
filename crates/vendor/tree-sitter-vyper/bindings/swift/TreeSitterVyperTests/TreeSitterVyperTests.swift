import XCTest
import SwiftTreeSitter
import TreeSitterVyper

final class TreeSitterVyperTests: XCTestCase {
    func testCanLoadGrammar() throws {
        let parser = Parser()
        let language = Language(language: tree_sitter_vyper())
        XCTAssertNoThrow(try parser.setLanguage(language),
                         "Error loading Vyper grammar")
    }
}
