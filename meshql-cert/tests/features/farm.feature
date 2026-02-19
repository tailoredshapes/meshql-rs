Feature: Farm E2E Certification

  Background:
    Given a MeshQL farm server is running
    And I have created "farm" entities:
      | name     | data                    |
      | Emerdale | {"name": "Emerdale"}    |
    And I have created "coop" entities:
      | name   | data                                                         |
      | red    | {"name": "red",    "farmId": "<ids.farm.Emerdale>"}          |
      | yellow | {"name": "yellow", "farmId": "<ids.farm.Emerdale>"}          |
    And I have created "hen" entities:
      | name  | data                                                                    |
      | chuck | {"name": "chuck", "eggs": 2, "coopId": "<ids.coop.red>"}               |
      | duck  | {"name": "duck",  "eggs": 0, "coopId": "<ids.coop.red>"}               |
      | vera  | {"name": "vera",  "eggs": 1, "coopId": "<ids.coop.yellow>"}            |
    And I capture the current timestamp as "first_stamp"
    And I update "coop" "red" with data {"name": "purple", "farmId": "<ids.farm.Emerdale>"}

  Scenario: Querying farm returns the correct name
    When I query the "farm" graph with: { getFarm(id: "<ids.farm.Emerdale>") { name } }
    Then there should be no GraphQL errors
    And the response data.getFarm.name should be "Emerdale"

  Scenario: Federated query returns farm with coops
    When I query the "farm" graph with: { getFarm(id: "<ids.farm.Emerdale>") { name coops { name } } }
    Then there should be no GraphQL errors
    And the response data.getFarm.name should be "Emerdale"
    And the response data.getFarm.coops should have 2 items
