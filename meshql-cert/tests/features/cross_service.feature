Feature: Cross-Service HTTP GraphQL Resolution

  Background:
    Given a cross-service MeshQL server pair is running
    And I have created "farm" entities:
      | name     | data                    |
      | Emerdale | {"name": "Emerdale"}    |
    And I have created "coop" entities on server B:
      | name   | data                                                         |
      | red    | {"name": "red",    "farmId": "<ids.farm.Emerdale>"}          |
      | yellow | {"name": "yellow", "farmId": "<ids.farm.Emerdale>"}          |

  Scenario: Singleton HTTP resolver resolves cross-service field
    When I query the "coop" graph on server B with: { getCoop(id: "<ids.coop.red>") { name farm { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getCoop.name" should be "red"
    And the response at "data.getCoop.farm.name" should be "Emerdale"

  Scenario: Vector HTTP resolver resolves cross-service field
    When I query the "farm" graph with: { getFarm(id: "<ids.farm.Emerdale>") { name coops { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getFarm.name" should be "Emerdale"
    And the response at "data.getFarm.coops" should have 2 items
