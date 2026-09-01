Feature: Repository Contract
  As a database plugin developer
  I want to implement the Repository interface correctly
  So that MeshQL can use any database backend reliably

  Background:
    Given a fresh repository instance

  Scenario: Creating a new envelope
    When I create envelopes:
      | name        | count |
      | Create Test | 3     |
    Then the envelopes should have generated IDs
    And the envelopes created_at should be greater than or equal to the test start time
    And the envelopes deleted flag should be disabled

  Scenario: Reading an envelope by ID
    Given I have created envelopes:
      | name      | count |
      | Read Test | 51    |
    When I read envelopes ["Read Test"] by their IDs
    Then I should receive 1 envelope
    And the payload "name" should be "Read Test"

  Scenario: Listing all envelopes
    Given I have created envelopes:
      | name  | count |
      | test1 | 4     |
      | test2 | 45    |
      | test3 | 2     |
    When I list all envelopes
    Then I should receive exactly 3 envelopes

  Scenario: Removing envelopes by ID
    Given I have created envelopes:
      | name      | count |
      | Read Test | 51    |
    When I remove envelope "Read Test"
    Then the remove operation should return true
    And reading envelopes ["Read Test"] by their IDs should return nothing

  Scenario: Reading multiple envelopes by IDs
    Given I have created envelopes:
      | name  | count |
      | test1 | 4     |
      | test2 | 45    |
      | test3 | 2     |
    When I read envelopes ["test1", "test2"] by their IDs
    Then I should receive exactly 2 envelopes

  Scenario: Removing multiple envelopes by IDs
    Given I have created envelopes:
      | name  | count |
      | test1 | 4     |
      | test2 | 45    |
      | test3 | 2     |
    When I remove envelopes ["test1", "test2"] by their IDs
    Then the remove operations should return true
    And listing all envelopes should show exactly 1 envelope

  Scenario: Temporal versioning - multiple versions of the same ID
    Given I create envelopes:
      | name | version | msg           |
      | doc1 | v1      | First version |
    And I capture the current timestamp as "t1"
    And I wait 50 milliseconds
    When I create a new version of envelope "doc1":
      | version | msg            |
      | v2      | Second version |
    And I capture the current timestamp as "t2"
    Then reading envelope "doc1" at timestamp "t2" should return version "v2"
    And reading envelope "doc1" at timestamp "t1" should return version "v1"

  Scenario: Listing only returns the latest version
    Given I create envelopes:
      | name | version | msg           |
      | doc1 | v1      | First version |
    And I wait 50 milliseconds
    When I create a new version of envelope "doc1":
      | version | msg            |
      | v2      | Second version |
    And I list all envelopes
    Then I should receive exactly 1 envelope
    And the payload "version" should be "v2"

  Scenario: Creating many envelopes in one call stores all of them
    When I create many envelopes:
      | name  | count |
      | bulk1 | 1     |
      | bulk2 | 2     |
      | bulk3 | 3     |
    Then the envelopes should have generated IDs
    And listing all envelopes should show exactly 3 envelopes

  Scenario: Listing excludes a removed envelope that has an earlier version
    Given I create envelopes:
      | name | version | msg           |
      | doc1 | v1      | First version |
    And I wait 50 milliseconds
    And I create a new version of envelope "doc1":
      | version | msg            |
      | v2      | Second version |
    When I remove envelope "doc1"
    Then the remove operation should return true
    And reading envelopes ["doc1"] by their IDs should return nothing
    And listing all envelopes should show exactly 0 envelopes
