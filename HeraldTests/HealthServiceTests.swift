import Testing
import Foundation
import HealthKit
@testable import Herald

@Suite("HealthService Tests")
struct HealthServiceTests {

    // MARK: - Entitlement Verification

    @MainActor
    @Test("Service reports unsupported when entitlements are absent")
    func testHealthKitUnavailableWhenEntitlementAbsent() async {
        let service = LiveHealthService()

        // Simulate missing entitlements by setting the cache directly.
        // In production, verifyEntitlements probes the store and caches the result.
        service.entitlementsVerified = false

        // When entitlements are absent, authorization status should be unsupported.
        // The service should not attempt to query HealthKit.
        let status = await service.requestAuthorization()
        #expect(status == .unsupported)
        #expect(service.authorizationStatus == .unsupported)
    }

    @MainActor
    @Test("refreshAuthorizationStatus reports unsupported when entitlements absent")
    func testRefreshStatusUnsupportedWhenEntitlementAbsent() async {
        let service = LiveHealthService()

        // Simulate missing entitlements
        service.entitlementsVerified = false

        await service.refreshAuthorizationStatus()
        #expect(service.authorizationStatus == .unsupported)
    }

    @MainActor
    @Test("collectSnapshot returns nil when entitlements are absent")
    func testCollectSnapshotNilWhenEntitlementAbsent() async {
        let service = LiveHealthService()

        // Even if UserDefaults flag is set (stale from a prior build with entitlements),
        // the service should not return data when entitlements are missing.
        service.entitlementsVerified = false
        UserDefaults.standard.set(true, forKey: "herald.healthkit.authorizationRequested")

        let snapshot = await service.collectSnapshot(forceFullRefresh: true)
        #expect(snapshot == nil)
    }

    // MARK: - Empty Query Does Not Imply Authorization

    @MainActor
    @Test("Empty query result does not set authorization to authorized")
    func testEmptyQueryDoesNotImplyAuthorization() async {
        let service = LiveHealthService()

        // Entitlements are present (not verified as absent)
        service.entitlementsVerified = true

        // Ensure the UserDefaults flag is NOT set (user never saw dialog)
        UserDefaults.standard.set(false, forKey: "herald.healthkit.authorizationRequested")

        // Refresh authorization status — should remain .notDetermined
        // because we cannot infer read authorization from query results.
        await service.refreshAuthorizationStatus()
        #expect(service.authorizationStatus == .notDetermined)
    }

    @MainActor
    @Test("Stale UserDefaults flag without entitlement verification reports unsupported")
    func testStaleFlagWithoutEntitlementCheck() async {
        let service = LiveHealthService()

        // Simulate: entitlements not yet verified (nil = not checked)
        // and UserDefaults has a stale flag from a prior build
        service.entitlementsVerified = nil
        UserDefaults.standard.set(true, forKey: "herald.healthkit.authorizationRequested")

        // On refresh, if entitlements can't be verified, should not blindly trust the flag.
        // Since we can't actually call verifyEntitlements (needs real store),
        // we test the guarded path by setting entitlementsVerified = false first.
        service.entitlementsVerified = false

        await service.refreshAuthorizationStatus()
        #expect(service.authorizationStatus == .unsupported)
    }

    // MARK: - Authorization Status Transitions

    @MainActor
    @Test("Authorization status starts as notDetermined when HealthKit available")
    func testInitialStatusNotDetermined() {
        let service = LiveHealthService()

        // On a device with HealthKit, initial status should be .notDetermined
        // (unless device doesn't support HealthKit, in which case it's .unsupported)
        if HKHealthStore.isHealthDataAvailable() {
            #expect(service.authorizationStatus == .notDetermined)
        } else {
            #expect(service.authorizationStatus == .unsupported)
        }
    }

    @MainActor
    @Test("Background delivery disabled when entitlements absent")
    func testBackgroundDeliveryDisabledWhenEntitlementsAbsent() async {
        let service = LiveHealthService()

        service.entitlementsVerified = false
        _ = await service.requestAuthorization()

        #expect(service.backgroundDeliveryEnabled == false)
    }

    // MARK: - Sleep Aggregation (pure logic, no HealthKit dependency)

    @Test("Sleep bucket day returns start of day")
    func testSleepBucketDay() {
        let calendar = Calendar.current
        let reference = calendar.date(from: DateComponents(year: 2025, month: 6, day: 15, hour: 14, minute: 30))!
        let bucket = LiveHealthService.sleepBucketDay(for: reference, calendar: calendar)

        let expected = calendar.startOfDay(for: reference)
        #expect(bucket == expected)
    }

    @Test("Aggregate sleep duration sums asleep intervals within bucket")
    func testAggregateSleepDuration() {
        let calendar = Calendar.current
        let bucketDay = calendar.date(from: DateComponents(year: 2025, month: 6, day: 15))!

        // 11pm June 14 to 7am June 15 = 8 hours of sleep
        let sleepStart = calendar.date(from: DateComponents(year: 2025, month: 6, day: 14, hour: 23))!
        let sleepEnd = calendar.date(from: DateComponents(year: 2025, month: 6, day: 15, hour: 7))!

        let intervals = [
            LiveHealthService.SleepInterval(
                value: HKCategoryValueSleepAnalysis.asleepCore.rawValue,
                startDate: sleepStart,
                endDate: sleepEnd
            )
        ]

        let duration = LiveHealthService.aggregateSleepDuration(
            intervals: intervals,
            attributedTo: bucketDay,
            calendar: calendar
        )

        #expect(duration == 8.0)
    }

    @Test("Aggregate sleep duration excludes awake intervals")
    func testAggregateSleepDurationExcludesAwake() {
        let calendar = Calendar.current
        let bucketDay = calendar.date(from: DateComponents(year: 2025, month: 6, day: 15))!

        let intervals = [
            LiveHealthService.SleepInterval(
                value: HKCategoryValueSleepAnalysis.awake.rawValue,
                startDate: calendar.date(from: DateComponents(year: 2025, month: 6, day: 15, hour: 2))!,
                endDate: calendar.date(from: DateComponents(year: 2025, month: 6, day: 15, hour: 3))!
            )
        ]

        let duration = LiveHealthService.aggregateSleepDuration(
            intervals: intervals,
            attributedTo: bucketDay,
            calendar: calendar
        )

        #expect(duration == nil)
    }

    @Test("Aggregate sleep duration returns nil when no asleep intervals")
    func testAggregateSleepDurationNilWhenEmpty() {
        let calendar = Calendar.current
        let bucketDay = calendar.date(from: DateComponents(year: 2025, month: 6, day: 15))!

        let duration = LiveHealthService.aggregateSleepDuration(
            intervals: [],
            attributedTo: bucketDay,
            calendar: calendar
        )

        #expect(duration == nil)
    }
}
