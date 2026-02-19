Feature: Egg Economy E2E Certification

  Background:
    Given a MeshQL egg economy server is running
    # Actors
    And I have created "farm" entities:
      | name        | data                                                                                          |
      | Green Acres | {"name": "Green Acres", "farm_type": "free_range", "zone": "north", "owner": "Old MacDonald"} |
    And I have created "coop" entities:
      | name    | data                                                                                                     |
      | Sunrise | {"name": "Sunrise", "farm_id": "<ids.farm.Green Acres>", "capacity": 20, "coop_type": "layer"}           |
      | Dusk    | {"name": "Dusk", "farm_id": "<ids.farm.Green Acres>", "capacity": 15, "coop_type": "free_range"}         |
    And I have created "hen" entities:
      | name      | data                                                                                                        |
      | Henrietta | {"name": "Henrietta", "coop_id": "<ids.coop.Sunrise>", "breed": "Leghorn", "status": "active"}              |
      | Clucky    | {"name": "Clucky", "coop_id": "<ids.coop.Sunrise>", "breed": "Rhode Island Red", "status": "active"}       |
      | Pecky     | {"name": "Pecky", "coop_id": "<ids.coop.Dusk>", "breed": "Sussex", "status": "active"}                     |
    And I have created "container" entities:
      | name       | data                                                                                       |
      | Cold Store | {"name": "Cold Store", "container_type": "refrigerator", "capacity": 500, "zone": "north"} |
      | Barn Box   | {"name": "Barn Box", "container_type": "crate", "capacity": 100, "zone": "north"}          |
    And I have created "consumer" entities:
      | name        | data                                                                                                |
      | Local Diner | {"name": "Local Diner", "consumer_type": "restaurant", "zone": "north", "weekly_demand": 100}       |

  # ===== ACTOR QUERIES =====

  Scenario: Query farm returns correct data
    When I query the "farm" graph with: { getById(id: "<ids.farm.Green Acres>") { name farm_type zone owner } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Green Acres"
    And the response at "data.getById.farm_type" should be "free_range"
    And the response at "data.getById.zone" should be "north"

  Scenario: Farm federates to coops
    When I query the "farm" graph with: { getById(id: "<ids.farm.Green Acres>") { name coops { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Green Acres"
    And the response at "data.getById.coops" should have 2 items

  Scenario: Coop federates to farm and hens
    When I query the "coop" graph with: { getById(id: "<ids.coop.Sunrise>") { name farm { name } hens { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Sunrise"
    And the response at "data.getById.farm.name" should be "Green Acres"
    And the response at "data.getById.hens" should have 2 items

  Scenario: Hen federates to coop
    When I query the "hen" graph with: { getById(id: "<ids.hen.Henrietta>") { name breed coop { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Henrietta"
    And the response at "data.getById.breed" should be "Leghorn"
    And the response at "data.getById.coop.name" should be "Sunrise"

  Scenario: Container query works
    When I query the "container" graph with: { getById(id: "<ids.container.Cold Store>") { name container_type capacity zone } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Cold Store"
    And the response at "data.getById.container_type" should be "refrigerator"

  Scenario: Consumer query works
    When I query the "consumer" graph with: { getById(id: "<ids.consumer.Local Diner>") { name consumer_type zone } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Local Diner"
    And the response at "data.getById.consumer_type" should be "restaurant"

  # ===== EVENT CREATION AND QUERIES =====

  Scenario: Lay report links to hen
    Given I have created "lay_report" entities:
      | name    | data                                                                                                                                                                            |
      | report1 | {"hen_id": "<ids.hen.Henrietta>", "coop_id": "<ids.coop.Sunrise>", "farm_id": "<ids.farm.Green Acres>", "eggs": 3, "timestamp": "2024-06-15T08:00:00Z", "quality": "grade_a"} |
    When I query the "lay_report" graph with: { getByHen(id: "<ids.hen.Henrietta>") { eggs quality hen { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getByHen" should have 1 item

  Scenario: Storage deposit links to container
    Given I have created "storage_deposit" entities:
      | name     | data                                                                                                                                                      |
      | deposit1 | {"container_id": "<ids.container.Cold Store>", "source_type": "coop", "source_id": "<ids.coop.Sunrise>", "eggs": 10, "timestamp": "2024-06-15T09:00:00Z"} |
    When I query the "storage_deposit" graph with: { getByContainer(id: "<ids.container.Cold Store>") { eggs container { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getByContainer" should have 1 item

  Scenario: Container transfer links source and dest
    Given I have created "container_transfer" entities:
      | name      | data                                                                                                                                                                                                    |
      | transfer1 | {"source_container_id": "<ids.container.Cold Store>", "dest_container_id": "<ids.container.Barn Box>", "eggs": 5, "timestamp": "2024-06-15T10:00:00Z", "transport_method": "hand_carry"} |
    When I query the "container_transfer" graph with: { getBySourceContainer(id: "<ids.container.Cold Store>") { eggs sourceContainer { name } destContainer { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getBySourceContainer" should have 1 item

  Scenario: Consumption report links consumer and container
    Given I have created "consumption_report" entities:
      | name         | data                                                                                                                                                                    |
      | consumption1 | {"consumer_id": "<ids.consumer.Local Diner>", "container_id": "<ids.container.Cold Store>", "eggs": 12, "timestamp": "2024-06-15T11:00:00Z", "purpose": "breakfast"} |
    When I query the "consumption_report" graph with: { getByConsumer(id: "<ids.consumer.Local Diner>") { eggs purpose consumer { name } container { name } } }
    Then there should be no GraphQL errors
    And the response at "data.getByConsumer" should have 1 item

  # ===== PROJECTION CREATION AND FEDERATION =====

  Scenario: Hen productivity links to hen
    Given I have created "hen_productivity" entities:
      | name  | data                                                                                                                                                                                               |
      | prod1 | {"hen_id": "<ids.hen.Henrietta>", "farm_id": "<ids.farm.Green Acres>", "eggs_today": 3, "eggs_week": 18, "eggs_month": 72, "avg_per_week": 18.0, "total_eggs": 500, "quality_rate": 0.95} |
    When I query the "hen_productivity" graph with: { getByHen(id: "<ids.hen.Henrietta>") { eggs_today eggs_week hen { name breed } } }
    Then there should be no GraphQL errors
    And the response at "data.getByHen" should have 1 item

  Scenario: Hen federates to productivity
    Given I have created "hen_productivity" entities:
      | name  | data                                                                                                                                                                                               |
      | prod2 | {"hen_id": "<ids.hen.Clucky>", "farm_id": "<ids.farm.Green Acres>", "eggs_today": 2, "eggs_week": 12, "eggs_month": 48, "avg_per_week": 12.0, "total_eggs": 300, "quality_rate": 0.90}    |
    When I query the "hen" graph with: { getById(id: "<ids.hen.Clucky>") { name productivity { eggs_today eggs_week } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Clucky"
    And the response at "data.getById.productivity" should have 1 item

  Scenario: Container inventory links to container
    Given I have created "container_inventory" entities:
      | name | data                                                                                                                                                                                                                               |
      | inv1 | {"container_id": "<ids.container.Cold Store>", "current_eggs": 50, "total_deposits": 100, "total_withdrawals": 30, "total_transfers_in": 10, "total_transfers_out": 20, "total_consumed": 10, "utilization_pct": 10.0} |
    When I query the "container" graph with: { getById(id: "<ids.container.Cold Store>") { name inventory { current_eggs total_deposits } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Cold Store"
    And the response at "data.getById.inventory" should have 1 item

  Scenario: Farm output links to farm
    Given I have created "farm_output" entities:
      | name    | data                                                                                                                                                                                                             |
      | output1 | {"farm_id": "<ids.farm.Green Acres>", "farm_type": "free_range", "eggs_today": 8, "eggs_week": 48, "eggs_month": 192, "active_hens": 3, "total_hens": 3, "avg_per_hen_per_week": 16.0}              |
    When I query the "farm" graph with: { getById(id: "<ids.farm.Green Acres>") { name farmOutput { eggs_today eggs_week active_hens } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Green Acres"
    And the response at "data.getById.farmOutput" should have 1 item

  # ===== DEEP FEDERATION CHAIN =====

  Scenario: Deep federation from farm through coops to hens
    When I query the "farm" graph with: { getById(id: "<ids.farm.Green Acres>") { name coops { name hens { name breed } } } }
    Then there should be no GraphQL errors
    And the response at "data.getById.name" should be "Green Acres"
    And the response at "data.getById.coops" should have 2 items

  # ===== TEMPORAL QUERIES =====

  Scenario: Temporal query returns old state after update
    Given I capture the current timestamp as "before_update"
    And I update "farm" "Green Acres" with data {"name": "Green Acres", "farm_type": "megafarm", "zone": "north", "owner": "Old MacDonald"}
    When I query the "farm" graph with: { getById(id: "<ids.farm.Green Acres>") { name farm_type } }
    Then there should be no GraphQL errors
    And the response at "data.getById.farm_type" should be "megafarm"
    When I query the "farm" graph with at=first_stamp: { getById(id: "<ids.farm.Green Acres>", at: first_stamp) { name farm_type } }
    Then there should be no GraphQL errors
    And the response at "data.getById.farm_type" should be "free_range"
