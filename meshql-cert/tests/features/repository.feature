Feature: Repository Contract

  Background:
    Given a fresh repository instance

  Scenario: Creating an envelope stores and returns it
    When I create envelopes named "Create Test"
    Then the envelopes should have generated IDs
    And the envelopes created_at should be recent
    And the envelopes deleted flag should be false

  Scenario: Reading an envelope by ID retrieves the correct data
    When I create envelopes named "Read Test"
    And I read the envelope named "Read Test"
    Then the read should succeed

  Scenario: Listing all envelopes returns created items
    When I create 3 envelopes named "List Item"
    And I list all envelopes
    Then the envelope list should contain at least 3 items
    And the envelope list should contain "List Item-0"
    And the envelope list should contain "List Item-1"
    And the envelope list should contain "List Item-2"

  Scenario: Removing an envelope soft-deletes it
    When I create envelopes named "To Delete"
    And I remove the envelope named "To Delete"
    Then the remove should return true
    And reading "To Delete" should return None

  Scenario: Creating many envelopes stores all of them
    When I create many envelopes with base name "Bulk Item" and count 3
    Then I should have 3 created envelopes

  Scenario: Reading many envelopes retrieves all of them
    When I create many envelopes with base name "ReadMany" and count 3
    And I read many envelopes named "ReadMany"
    Then I should have 3 read envelopes

  Scenario: Removing many envelopes deletes all of them
    When I create many envelopes with base name "RemoveMany" and count 3
    And I remove many envelopes named "RemoveMany"
    Then all removes should succeed

  Scenario: Temporal versioning allows reading old versions
    When I create a version 1 envelope named "Temporal" with value "version-1" dated 10 seconds ago
    And I create a version 2 envelope for "Temporal" with value "version-2"
    And I read envelope "Temporal" at timestamp "before_Temporal"
    Then the result at "before_Temporal" should have version "version-1"
    When I read envelope "Temporal" now
    Then the result at "before_Temporal" should have version "version-2"

  Scenario: Listing only shows the latest version per ID
    When I create two versions of envelope "Latest" with old value "old" and new value "new"
    And I list all envelopes
    Then listing should return exactly 1 result for "Latest"
    And the listed version should have value "new"
