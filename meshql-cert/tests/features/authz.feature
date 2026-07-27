Feature: End-to-end authorization with differentiated credentials

  meshql's contract is that the edge authenticates and the framework
  authorizes. Every scenario below drives the real HTTP surfaces — writes go
  through the restlette, reads through the graphlette — with credentials
  supplied the way a deployment supplies them: a trusted identity header the
  edge lifts into the request's AuthContext.

  Callers "alice" and "bob" hold distinct, non-wildcard credentials. That is
  the point of this suite: the older certifications authenticate with the
  wildcard token "*", and against a caller entitled to everything a working
  authorization filter and an inert one return exactly the same rows. Two
  scenarios here do use the wildcard caller ("root") and the credential-less
  caller ("anonymous") — because those two behaviours are themselves part of
  the contract and must not regress.

  Background:
    Given an authorizing MeshQL server is running

  Scenario: One caller's record is invisible to another
    When "alice" creates a widget "alpha" of kind "tool"
    Then "alice" can read widget "alpha" by id
    And "bob" cannot read widget "alpha" by id

  Scenario: A list query returns only the caller's own records
    When "alice" creates a widget "alpha" of kind "tool"
    And "bob" creates a widget "beta" of kind "tool"
    When "alice" queries widgets of kind "tool"
    Then the result should be exactly "alpha"
    When "bob" queries widgets of kind "tool"
    Then the result should be exactly "beta"
    When "alice" lists all widgets
    Then the result should be exactly "alpha"
    When "bob" lists all widgets
    Then the result should be exactly "beta"

  Scenario: A record written without credentials stays public
    When "anonymous" creates a widget "commons" of kind "tool"
    Then the stored envelope for widget "commons" should carry no tokens
    And "alice" can read widget "commons" by id
    And "bob" can read widget "commons" by id
    And "anonymous" can read widget "commons" by id

  Scenario: A wildcard caller still sees everything
    When "alice" creates a widget "alpha" of kind "tool"
    And "bob" creates a widget "beta" of kind "tool"
    Then "root" can read widget "alpha" by id
    And "root" can read widget "beta" by id
    When "root" queries widgets of kind "tool"
    Then the result should be exactly "alpha, beta"

  Scenario: The tokens the Auth resolved are the tokens that get stored
    When "alice" creates a widget "alpha" of kind "tool"
    Then the stored envelope for widget "alpha" should carry the tokens "alice" resolves to
    When "bob" creates a widget "beta" of kind "tool"
    Then the stored envelope for widget "beta" should carry the tokens "bob" resolves to

  Scenario: Delete honours tokens
    When "alice" creates a widget "alpha" of kind "tool"
    And "bob" deletes widget "alpha"
    Then the delete should be refused
    And "alice" can read widget "alpha" by id
    When "alice" deletes widget "alpha"
    Then the delete should succeed
    And "alice" cannot read widget "alpha" by id

  Scenario: A rewound clock does not rewind authorization
    When "alice" creates a widget "alpha" of kind "tool"
    And the current instant is captured as "after_alpha"
    And "bob" creates a widget "beta" of kind "tool"
    When "alice" reads widget "alpha" by id as of "after_alpha"
    Then the temporal read should find it
    When "bob" reads widget "alpha" by id as of "after_alpha"
    Then the temporal read should find nothing
    When "alice" queries widgets of kind "tool" as of "after_alpha"
    Then the result should be exactly "alpha"
    When "bob" queries widgets of kind "tool" as of "after_alpha"
    Then the result should be exactly ""
