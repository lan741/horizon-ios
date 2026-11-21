// Combine shim — type-check only, never shipped.
//
// A Windows or Linux Swift toolchain has no Apple frameworks, so files that use
// `ObservableObject` / `@Published` cannot be type checked at all. This module
// declares just enough of Combine for the compiler to check *our* code: the
// property wrapper, the publisher, and `objectWillChange`.
//
// It is deliberately not a behavioural emulation. It exists to catch wrong
// member names, wrong types and missing imports in the model layer on a machine
// that cannot build the app. Anything that depends on real Combine semantics is
// still only proven by the macOS CI build.

import Foundation

/// Stand-in for `Combine.ObservableObject`.
public protocol ObservableObject: AnyObject {}

/// Stand-in for `Combine.ObservableObjectPublisher`.
public final class ObservableObjectPublisher {
    public init() {}
    public func send() {}
}

public extension ObservableObject {
    /// Real Combine synthesises this per conforming type; one shared instance is
    /// enough for type checking because nothing here observes it.
    var objectWillChange: ObservableObjectPublisher { ObservableObjectPublisher() }
}

/// Stand-in for `Combine.Published`.
@propertyWrapper
public struct Published<Value> {
    public var wrappedValue: Value {
        get { storage }
        set { storage = newValue }
    }

    public var projectedValue: Published<Value>.Publisher { Publisher() }

    private var storage: Value

    public init(wrappedValue: Value) {
        self.storage = wrappedValue
    }

    public init(initialValue: Value) {
        self.storage = initialValue
    }

    /// `$property` on a `@Published` value is a publisher; our code only reads
    /// it through SwiftUI bindings, which this shim never sees.
    public struct Publisher {
        public init() {}
    }
}

/// Stand-in for the parts of `AnyPublisher`/`PassthroughSubject` our code names.
public final class PassthroughSubject<Output, Failure: Error> {
    public init() {}
    public func send(_ value: Output) {}
}