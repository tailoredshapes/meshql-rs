Feature: Searcher Contract

  Background:
    Given a fresh repository instance
    And the searcher dataset is seeded

  Scenario: Finding a nonexistent ID returns empty
    When I search using literal template '{"id": "nonexistent-id"}'
    Then the search result should be empty

  Scenario: Finding by ID returns the correct item
    When I search using template "findById" with arg "id" = "alpha"
    Then the search result should not be empty
    And the search result should have "id" = "s-id-1"
    And the search result should have "name" = "alpha"

  Scenario: Finding by name returns the correct item
    When I search using template "findByName" with arg "id" = "beta"
    Then the search result should not be empty
    And the search result should have "name" = "beta"

  Scenario: Finding all by type returns the correct items
    When I search all using template "findAllByType" with arg "id" = "typeA"
    Then the search results count should be 2
    And all search results should have "type" = "typeA"

  Scenario: Finding all by type and name returns a single item
    When I search all using template "findByNameAndType" with args: name=delta, type=typeB
    Then the search results count should be 1
    And all search results should have "name" = "delta"

  Scenario: Finding all for a nonexistent type returns empty
    When I search all using template "findAllByType" with arg "id" = "typeZ"
    Then the search results should be empty

  Scenario: Searching with a limit respects the limit
    When I search all using literal template '{}' with limit 1
    Then the search results count should be 1

  Scenario: Searching with an empty query returns all items
    When I search all using literal template '{}'
    Then the search results should not be empty
